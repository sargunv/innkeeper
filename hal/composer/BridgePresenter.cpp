// SPDX-License-Identifier: AGPL-3.0-or-later
#define LOG_TAG "linuxapk-hwc"
#include <algorithm>
#include <atomic>
#include <deque>
#include <map>
#include <mutex>
#include <optional>
#include <thread>
#include <vector>

#include <errno.h>
#include <fcntl.h>
#include <linux/udmabuf.h>
#include <poll.h>
#include <pthread.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

#include "BridgePresenter.h"

#include "InputServer.h"
#include "Properties.h"
#include "linuxapk/layout.h"
#include "linuxapk/log.h"

namespace linuxapk::hwc {

namespace {

// docs/bridge.md: protocol v1 framing and the display service (id 2).
constexpr uint16_t kHello = 0x0001;
constexpr uint16_t kReply = 0x8000;
constexpr uint32_t kServiceDisplay = 2;
constexpr uint16_t kWindowCreate = 0x0201;
constexpr uint16_t kWindowDestroy = 0x0202;
constexpr uint16_t kWindowSetTitle = 0x0203;
constexpr uint16_t kWindowSetMinSize = 0x0204;
constexpr uint16_t kWindowActivate = 0x0205;
constexpr uint16_t kBufferAdd = 0x0206;
constexpr uint16_t kBufferRemove = 0x0207;
constexpr uint16_t kFrame = 0x0208;
constexpr uint16_t kTextInput = 0x0209;
constexpr size_t kMaxTextInputLine = 480;
constexpr uint16_t kEvConfigure = 0x4201;
constexpr uint16_t kEvClose = 0x4202;
constexpr uint16_t kEvRelease = 0x4203;
constexpr uint16_t kEvInput = 0x4204;
constexpr size_t kMaxMsg = 512;
constexpr size_t kMaxName = 96;
constexpr int kPoolSize = 3;
constexpr int32_t kBaseDpi = 160;
constexpr int64_t kPendingTimeoutNs = 10'000'000'000;
constexpr uint32_t kXBGR8888 = fourcc('X', 'B', '2', '4');  // R,G,B,X bytes
constexpr uint32_t kXRGB8888 = fourcc('X', 'R', '2', '4');  // B,G,R,X bytes

struct Msg {
  std::vector<uint8_t> b;
  Msg(uint16_t type, uint32_t tag) {
    u16(type);
    u16(0);
    u32(tag);
  }
  Msg& u16(uint16_t v) {
    b.push_back(uint8_t(v));
    b.push_back(uint8_t(v >> 8));
    return *this;
  }
  Msg& u32(uint32_t v) {
    for (int i = 0; i < 4; i++) b.push_back(uint8_t(v >> (8 * i)));
    return *this;
  }
  Msg& u64(uint64_t v) {
    for (int i = 0; i < 8; i++) b.push_back(uint8_t(v >> (8 * i)));
    return *this;
  }
  // Bounded UTF-8 without control characters (the bridge refuses others).
  Msg& name(const std::string& s) {
    std::string t;
    for (unsigned char c : s) {
      if (c >= 0x20 && c != 0x7f) t.push_back(char(c));
    }
    if (t.size() > kMaxName) {
      size_t n = kMaxName;
      while (n > 0 && (uint8_t(t[n]) & 0xc0) == 0x80) n--;  // UTF-8 boundary
      t.resize(n);
    }
    u16(uint16_t(t.size()));
    b.insert(b.end(), t.begin(), t.end());
    return *this;
  }
};

uint32_t rd32(const uint8_t* p) {
  return uint32_t(p[0]) | uint32_t(p[1]) << 8 | uint32_t(p[2]) << 16 |
         uint32_t(p[3]) << 24;
}

std::string sanitize(std::string s) {
  for (char& ch : s) {
    if (ch == '\n' || ch == '\r') ch = ' ';
  }
  return s;
}

struct Spec {
  int32_t id = 0;
  uint32_t width = 0, height = 0;  // display pixels
  int32_t dpi = kBaseDpi;
  int32_t minW = 0, minH = 0;
  std::string appId, title;
};

}  // namespace

struct BridgeBuffer {
  uint32_t id = 0;
  int memfd = -1;
  uint8_t* map = nullptr;
  size_t size = 0;
  bool busy = false;  // sent in a FRAME, not yet released
};

struct Pool {
  std::vector<BridgeBuffer> bufs;
  uint32_t width = 0, height = 0, stride = 0, format = 0;
};

class Bridge;

class BridgeWindow {
 public:
  BridgeWindow(Bridge* b, Spec s, int64_t hwcId)
      : b(b), mSpec(std::move(s)), mHwcId(hwcId) {}
  ~BridgeWindow();
  int32_t id() const { return mSpec.id; }
  int64_t hwcId() const { return mHwcId; }
  const Spec& spec() const { return mSpec; }
  // Logical size the window starts at: the display's size in dp.
  void initialSize(uint32_t* w, uint32_t* h) const {
    const double k = double(kBaseDpi) / std::max(mSpec.dpi, 1);
    *w = std::max(1u, uint32_t(mSpec.width * k + 0.5));
    *h = std::max(1u, uint32_t(mSpec.height * k + 0.5));
  }
  Error present(FrameInput& frame);
  // Bridge lock held.
  void onRelease(uint32_t buffer);
  void dropPool();  // bridge reconnected: buffers must be added again
  void setMinSize(int32_t w, int32_t h) {
    mSpec.minW = w;
    mSpec.minH = h;
  }

 private:
  bool makePool(uint32_t w, uint32_t h, uint32_t format, Pool* out);
  void freePool(Pool& p, bool tellBridge);
  Bridge* b;
  Spec mSpec;
  const int64_t mHwcId;
  Pool mPool;  // guarded by the bridge lock (busy flags)
  int mLast = -1;
  uint64_t mFrames = 0, mDropped = 0;
  int64_t mLastLogNs = 0;
  bool mWarnedFormat = false;
};

class Bridge {
 public:
  InputServer input;
  std::mutex mutex;  // windows, pending, pools
  std::map<int32_t, std::shared_ptr<BridgeWindow>> windows;
  struct Pending {
    Spec spec;
    int64_t time;
  };
  std::deque<Pending> pending;
  std::atomic<uint32_t> nextBuffer{1};

  bool connect(const std::string& path);
  bool send(Msg& m, int fd = -1);
  uint32_t tag() { return mTag.fetch_add(1) + 1; }
  void createWindow(const BridgeWindow& w);
  void start() {
    mThread = std::thread([this] { loop(); });
  }
  bool connected() const { return mSock.load() >= 0; }
  void onCommand(const std::string& line);
  std::optional<Spec> claimPending(uint32_t w, uint32_t h);
  std::shared_ptr<BridgeWindow> findWindow(int32_t id, int64_t hwcId);
  void removeWindow(int32_t id, int64_t hwcId);

 private:
  void loop();
  void handle(const uint8_t* m, size_t n);
  std::string mPath;
  std::atomic<int> mSock{-1};
  std::mutex mSendMutex;
  std::atomic<uint32_t> mTag{1};
  std::map<uint32_t, int32_t> mCreates;  // tag of WINDOW_CREATE -> window
  std::thread mThread;
};

static Bridge* gBridge = nullptr;

// ---- connection
// ------------------------------------------------------------------

bool Bridge::connect(const std::string& path) {
  mPath = path;
  int fd = socket(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0);
  sockaddr_un addr{};
  addr.sun_family = AF_UNIX;
  if (fd < 0 || path.size() >= sizeof(addr.sun_path)) {
    if (fd >= 0) close(fd);
    return false;
  }
  memcpy(addr.sun_path, path.c_str(), path.size() + 1);
  if (::connect(fd, reinterpret_cast<sockaddr*>(&addr), sizeof addr) != 0) {
    LOGW("bridge: connect %s: %s", path.c_str(), strerror(errno));
    close(fd);
    return false;
  }
  Msg hello(kHello, 1);
  hello.u16(1).u16(0).u32(kServiceDisplay).name("composer");
  uint8_t buf[kMaxMsg];
  ssize_t n = -1;
  if (
    ::send(fd, hello.b.data(), hello.b.size(), MSG_NOSIGNAL) ==
    ssize_t(hello.b.size())
  ) {
    pollfd p{fd, POLLIN, 0};
    if (poll(&p, 1, 5000) == 1) n = recv(fd, buf, sizeof buf, 0);
  }
  // Reply: type, flags, tag, then i32 status.
  if (
    n < 12 || (buf[0] | buf[1] << 8) != kReply || int32_t(rd32(buf + 8)) != 0
  ) {
    LOGW(
      "bridge: the display service refused us (%s)",
      n < 12 ? "no reply" : std::to_string(int32_t(rd32(buf + 8))).c_str()
    );
    close(fd);
    return false;
  }
  mSock = fd;
  LOGI("bridge: display service on %s", path.c_str());
  return true;
}

bool Bridge::send(Msg& m, int fd) {
  int s = mSock.load();
  if (s < 0 || m.b.size() > kMaxMsg) return false;
  iovec iov{m.b.data(), m.b.size()};
  msghdr h{};
  h.msg_iov = &iov;
  h.msg_iovlen = 1;
  alignas(cmsghdr) char ctl[CMSG_SPACE(sizeof(int))];
  if (fd >= 0) {
    h.msg_control = ctl;
    h.msg_controllen = sizeof ctl;
    cmsghdr* c = CMSG_FIRSTHDR(&h);
    c->cmsg_level = SOL_SOCKET;
    c->cmsg_type = SCM_RIGHTS;
    c->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(c), &fd, sizeof fd);
  }
  std::lock_guard lock(mSendMutex);
  if (sendmsg(s, &h, MSG_NOSIGNAL) != ssize_t(m.b.size())) {
    LOGW("bridge: send: %s", strerror(errno));
    return false;
  }
  return true;
}

void Bridge::createWindow(const BridgeWindow& w) {
  uint32_t lw, lh;
  w.initialSize(&lw, &lh);
  const uint32_t t = tag();
  Msg m(kWindowCreate, t);
  m.u32(uint32_t(w.id()))
    .u32(lw)
    .u32(lh)
    .u32(uint32_t(std::max(0, w.spec().minW)))
    .u32(uint32_t(std::max(0, w.spec().minH)))
    .name(w.spec().appId)
    .name(w.spec().title);
  {
    std::lock_guard lock(mutex);
    mCreates[t] = w.id();
  }
  send(m);
}

void Bridge::handle(const uint8_t* m, size_t n) {
  if (n < 8) return;
  const uint16_t type = uint16_t(m[0] | m[1] << 8);
  const uint8_t* body = m + 8;
  const size_t len = n - 8;
  if (type == kReply) {
    if (len < 4) return;
    const uint32_t t = rd32(m + 4);
    const int32_t status = int32_t(rd32(body));
    int32_t window = -1;
    {
      std::lock_guard lock(mutex);
      auto it = mCreates.find(t);
      if (it != mCreates.end()) {
        window = it->second;
        mCreates.erase(it);
      }
    }
    if (status != 0) {
      LOGW("bridge: request %u failed: %d", t, status);
      if (window >= 0) input.send("window_failed %d bridge", window);
    }
    return;
  }
  switch (type) {
    case kEvConfigure:
      if (len >= 16) {
        const int32_t id = int32_t(rd32(body));
        input.setWindow(
          id, int32_t(rd32(body + 4)), int32_t(rd32(body + 8)),
          rd32(body + 12) / 120.0
        );
      }
      break;
    case kEvClose:
      if (len >= 4) input.send("close %d", int32_t(rd32(body)));
      break;
    case kEvRelease:
      if (len >= 4) {
        const uint32_t buffer = rd32(body);
        std::lock_guard lock(mutex);
        for (auto& [id, w] : windows) w->onRelease(buffer);
      }
      break;
    case kEvInput:
      if (len >= 2) {
        const size_t l = std::min<size_t>(body[0] | body[1] << 8, len - 2);
        std::string line(reinterpret_cast<const char*>(body + 2), l);
        // The bridge sends protocol v2 lines; relay them as they are.
        input.send("%s", sanitize(line).c_str());
      }
      break;
    default:
      break;
  }
}

void Bridge::loop() {
  pthread_setname_np(pthread_self(), "hwc-bridge");
  uint8_t buf[kMaxMsg + 1];
  while (true) {
    const int s = mSock.load();
    if (s >= 0) {
      ssize_t n = recv(s, buf, sizeof buf, 0);
      if (n > 0) {
        handle(buf, size_t(n));
        continue;
      }
      if (n < 0 && errno == EINTR) continue;
      LOGW("bridge: connection lost; reconnecting");
      mSock = -1;
      close(s);
    }
    sleep(1);
    if (!connect(mPath)) continue;
    // The bridge restarted: its windows and buffers are gone. Ask again.
    std::vector<std::shared_ptr<BridgeWindow>> ws;
    {
      std::lock_guard lock(mutex);
      for (auto& [id, w] : windows) {
        w->dropPool();
        ws.push_back(w);
      }
    }
    for (auto& w : ws) createWindow(*w);
  }
}

// ---- window registry (same commands as the direct Wayland backend)
// --------------

void Bridge::onCommand(const std::string& line) {
  // The Linux.apk IME's text-input lines ("ti_..."): the bridge parses and
  // validates them (docs/bridge.md, "Text input"); relay them as they are.
  if (line.rfind("ti_", 0) == 0) {
    if (line.size() > kMaxTextInputLine) return;
    Msg m(kTextInput, tag());
    m.u16(uint16_t(line.size()));
    m.b.insert(m.b.end(), line.begin(), line.end());
    send(m);
    return;
  }
  char cmd[32] = {};
  int32_t id = -1;
  int n = 0;
  if (sscanf(line.c_str(), "%31s %d%n", cmd, &id, &n) < 1) return;
  std::string op = cmd;
  if (op == "ping") {
    input.send(
      "pong %s", line.size() > 5 ? sanitize(line.substr(5)).c_str() : ""
    );
    return;
  }
  if (id < 0) return;
  const char* rest = line.c_str() + n;
  while (*rest == ' ') rest++;
  if (op == "window_new") {
    Spec spec;
    spec.id = id;
    char appId[256] = {};
    int m = 0;
    if (
      sscanf(
        rest, "%u %u %d %255s%n", &spec.width, &spec.height, &spec.dpi, appId,
        &m
      ) < 4 ||
      spec.width < 1 || spec.height < 1 || spec.dpi < 1
    ) {
      input.send("window_failed %d bad_request", id);
      return;
    }
    spec.appId = appId;
    const char* title = rest + m;
    while (*title == ' ') title++;
    spec.title = sanitize(*title ? title : appId);
    {
      std::lock_guard lock(mutex);
      if (windows.count(id)) {
        input.send("window_failed %d exists", id);
        return;
      }
      const int64_t now = nowNs();
      while (!pending.empty() &&
             now - pending.front().time > kPendingTimeoutNs) {
        input.send("window_failed %d timeout", pending.front().spec.id);
        pending.pop_front();
      }
      std::erase_if(pending, [&](auto& p) { return p.spec.id == id; });
      pending.push_back({spec, now});
    }
    LOGI(
      "wm: window %d requested: %ux%u @ %d dpi, %s", id, spec.width,
      spec.height, spec.dpi, spec.appId.c_str()
    );
    input.send("window_pending %d", id);
  } else if (op == "window_min_size") {
    int32_t w = 0, h = 0;
    if (
      sscanf(rest, "%d %d", &w, &h) != 2 || w < 0 || h < 0 || w > 16384 ||
      h > 16384
    )
      return;
    {
      std::lock_guard lock(mutex);
      for (auto& p : pending) {
        if (p.spec.id == id) {
          p.spec.minW = w;
          p.spec.minH = h;
        }
      }
      auto it = windows.find(id);
      if (it == windows.end()) return;
      it->second->setMinSize(w, h);
    }
    Msg m(kWindowSetMinSize, tag());
    m.u32(uint32_t(id)).u32(uint32_t(w)).u32(uint32_t(h));
    send(m);
  } else if (op == "window_title") {
    if (!findWindow(id, -1)) return;
    Msg m(kWindowSetTitle, tag());
    m.u32(uint32_t(id)).name(sanitize(rest));
    send(m);
  } else if (op == "window_activate") {
    if (!findWindow(id, -1)) return;
    Msg m(kWindowActivate, tag());
    m.u32(uint32_t(id));
    send(m);
  } else if (op == "window_destroy") {
    {
      std::lock_guard lock(mutex);
      std::erase_if(pending, [&](auto& p) { return p.spec.id == id; });
    }
    removeWindow(id, -1);
  }
}

std::optional<Spec> Bridge::claimPending(uint32_t w, uint32_t h) {
  std::lock_guard lock(mutex);
  const int64_t now = nowNs();
  for (auto it = pending.begin(); it != pending.end(); ++it) {
    if (now - it->time > kPendingTimeoutNs) continue;
    if (it->spec.width == w && it->spec.height == h) {
      Spec s = it->spec;
      pending.erase(it);
      return s;
    }
  }
  return std::nullopt;
}

std::shared_ptr<BridgeWindow> Bridge::findWindow(int32_t id, int64_t hwcId) {
  std::lock_guard lock(mutex);
  auto it = windows.find(id);
  if (it == windows.end()) return nullptr;
  if (hwcId != -1 && it->second->hwcId() != hwcId) return nullptr;
  return it->second;
}

void Bridge::removeWindow(int32_t id, int64_t hwcId) {
  std::shared_ptr<BridgeWindow> gone;
  {
    std::lock_guard lock(mutex);
    auto it = windows.find(id);
    if (it == windows.end()) return;
    if (hwcId != -1 && it->second->hwcId() != hwcId) return;
    gone = std::move(it->second);
    windows.erase(it);
  }
  input.removeWindow(id);
  Msg m(kWindowDestroy, tag());
  m.u32(uint32_t(id));
  send(m);  // the bridge drops the window's buffers with it
  gone.reset();
  LOGI("wm: window %d closed", id);
  input.send("window_gone %d", id);
}

// ---- window
// ----------------------------------------------------------------------

BridgeWindow::~BridgeWindow() {
  std::lock_guard lock(b->mutex);
  freePool(mPool, /*tellBridge=*/false);
}

void BridgeWindow::freePool(Pool& p, bool tellBridge) {
  for (auto& buf : p.bufs) {
    if (tellBridge) {
      Msg m(kBufferRemove, b->tag());
      m.u32(buf.id);
      b->send(m);
    }
    if (buf.map) munmap(buf.map, buf.size);
    if (buf.memfd >= 0) close(buf.memfd);
  }
  p.bufs.clear();
  p.width = p.height = 0;
}

void BridgeWindow::dropPool() {
  freePool(mPool, /*tellBridge=*/false);
  mLast = -1;
}

void BridgeWindow::onRelease(uint32_t buffer) {
  for (auto& buf : mPool.bufs) {
    if (buf.id == buffer) buf.busy = false;
  }
}

bool BridgeWindow::makePool(
  uint32_t w, uint32_t h, uint32_t format, Pool* out
) {
  Pool pool;
  pool.width = w;
  pool.height = h;
  pool.format = format;
  pool.stride = uint32_t(alignUp(uint64_t(w) * 4, 256));
  const size_t size = alignUp(uint64_t(pool.stride) * h, kPageSize);
  bool ok = true;
  for (int i = 0; i < kPoolSize && ok; i++) {
    BridgeBuffer buf;
    buf.size = size;
    buf.id = b->nextBuffer.fetch_add(1);
    buf.memfd = memfd_create("linuxapk-frame", MFD_CLOEXEC | MFD_ALLOW_SEALING);
    ok = buf.memfd >= 0 && ftruncate(buf.memfd, off_t(size)) == 0;
    if (ok) {
      void* p =
        mmap(nullptr, size, PROT_READ | PROT_WRITE, MAP_SHARED, buf.memfd, 0);
      ok = p != MAP_FAILED;
      if (ok) buf.map = static_cast<uint8_t*>(p);
    }
    int dmabuf = -1;
    if (ok) {
      fcntl(buf.memfd, F_ADD_SEALS, F_SEAL_SHRINK);
      int dev = open("/dev/udmabuf", O_RDWR | O_CLOEXEC);
      udmabuf_create create{};
      create.memfd = uint32_t(buf.memfd);
      create.flags = UDMABUF_FLAGS_CLOEXEC;
      create.size = size;
      dmabuf = dev >= 0 ? ioctl(dev, UDMABUF_CREATE, &create) : -1;
      if (dev >= 0) close(dev);
      ok = dmabuf >= 0;
    }
    if (ok) {
      Msg m(kBufferAdd, b->tag());
      m.u32(buf.id)
        .u32(uint32_t(mSpec.id))
        .u32(w)
        .u32(h)
        .u32(format)
        .u32(pool.stride)
        .u32(0)
        .u64(0);  // LINEAR
      ok = b->send(m, dmabuf);
    }
    if (dmabuf >= 0) close(dmabuf);
    pool.bufs.push_back(buf);
  }
  if (!ok) {
    freePool(pool, /*tellBridge=*/true);
    return false;
  }
  *out = std::move(pool);
  return true;
}

Error BridgeWindow::present(FrameInput& frame) {
  ClientTargetState* ct = frame.clientTarget;
  BufferPtr buffer = ct && ct->buffer ? ct->buffer : frame.outputBuffer;
  if (!buffer) return Error::None;
  const BufferHandle* bh = buffer->info();
  const uint8_t* src = buffer->map();
  if (!bh || !src) return Error::None;
  const int32_t f = bh->resolvedFormat;
  uint32_t format;
  if (f == fmt::RGBA_8888 || f == fmt::RGBX_8888) {
    format = kXBGR8888;
  } else if (f == fmt::BGRA_8888) {
    format = kXRGB8888;
  } else {
    if (!mWarnedFormat) LOGW("bridge: frame format 0x%x not supported yet", f);
    mWarnedFormat = true;
    return Error::None;
  }
  if (ct && ct->buffer == buffer && !waitFence(ct->acquireFence.get(), 1000))
    LOGW("bridge: client target fence timeout");
  src += bh->planes[0].offset.get();
  const uint32_t srcStride = bh->planes[0].stride;
  const uint32_t w = mHwcId >= 0 ? bh->width : std::min(bh->width, mSpec.width);
  const uint32_t h =
    mHwcId >= 0 ? bh->height : std::min(bh->height, mSpec.height);
  if (w == 0 || h == 0 || !b->connected()) return Error::None;

  if (w != mPool.width || h != mPool.height || format != mPool.format) {
    Pool np;
    if (!makePool(w, h, format, &np)) {
      LOGE("bridge: window %d: no %ux%u buffers", mSpec.id, w, h);
      return Error::None;
    }
    std::lock_guard lock(b->mutex);
    freePool(mPool, /*tellBridge=*/true);
    mPool = std::move(np);
    mLast = -1;
  }

  int idx = -1;
  uint32_t id = 0;
  {
    std::lock_guard lock(b->mutex);
    mFrames++;
    for (size_t i = 0; i < mPool.bufs.size(); i++) {
      if (!mPool.bufs[i].busy && int(i) != mLast) {
        idx = int(i);
        break;
      }
    }
    if (idx < 0) {
      mDropped++;  // every buffer is still on screen or queued
      return Error::None;
    }
    mPool.bufs[size_t(idx)].busy = true;
    id = mPool.bufs[size_t(idx)].id;
  }
  uint8_t* dst = mPool.bufs[size_t(idx)].map;
  for (uint32_t y = 0; y < h; y++) {
    memcpy(
      dst + size_t(y) * mPool.stride, src + size_t(y) * srcStride, size_t(w) * 4
    );
  }
  mLast = idx;
  Msg m(kFrame, b->tag());
  m.u32(uint32_t(mSpec.id)).u32(id).u32(0).u32(0).u32(0).u32(0);
  if (!b->send(m)) {
    std::lock_guard lock(b->mutex);
    mPool.bufs[size_t(idx)].busy = false;
  }
  const int64_t now = nowNs();
  if (now - mLastLogNs > 60'000'000'000) {
    LOGI(
      "bridge: window %d: %llu frames, %llu dropped (all buffers busy), frame "
      "%ux%u",
      mSpec.id, (unsigned long long)mFrames, (unsigned long long)mDropped, w, h
    );
    mLastLogNs = now;
  }
  return Error::None;
}

// ---- presenters
// ------------------------------------------------------------------

namespace {
class BridgeWindowPresenter : public Presenter {
 public:
  BridgeWindowPresenter(
    int32_t id, int64_t hwcId, std::unique_ptr<HeadlessPresenter> secondary
  )
      : mId(id), mHwcId(hwcId), mSecondary(std::move(secondary)) {}
  ~BridgeWindowPresenter() override { gBridge->removeWindow(mId, mHwcId); }
  const char* name() const override { return "bridge"; }
  Error present(FrameInput& frame, PresentResult* out) override {
    if (mSecondary) {
      PresentResult ignored;
      mSecondary->present(frame, &ignored);
    }
    out->presentFence.reset();
    if (auto w = gBridge->findWindow(mId, mHwcId)) return w->present(frame);
    return Error::None;
  }

 private:
  const int32_t mId;
  const int64_t mHwcId;
  std::unique_ptr<HeadlessPresenter> mSecondary;
};

std::unique_ptr<Presenter> addWindow(
  Spec spec, int64_t hwcId, std::unique_ptr<HeadlessPresenter> secondary
) {
  auto w = std::make_shared<BridgeWindow>(gBridge, spec, hwcId);
  std::shared_ptr<BridgeWindow> old;
  {
    std::lock_guard lock(gBridge->mutex);
    old = std::move(gBridge->windows[spec.id]);
    gBridge->windows[spec.id] = w;
  }
  old.reset();
  gBridge->createWindow(*w);
  if (hwcId >= 0)
    gBridge->input.send("window_mapped %d %lld", spec.id, (long long)hwcId);
  if (secondary && hwcId >= 0) {
    char prefix[32];
    snprintf(prefix, sizeof prefix, "window%d", spec.id);
    secondary->setDumpPrefix(prefix);
  }
  return std::make_unique<BridgeWindowPresenter>(
    spec.id, hwcId, std::move(secondary)
  );
}
}  // namespace

bool initBridge(
  const std::string& bridgeSocket, const std::string& inputSocket
) {
  auto* b = new Bridge();
  if (!b->connect(bridgeSocket)) {
    delete b;
    return false;
  }
  gBridge = b;
  b->input.setCommandHandler([b](const std::string& l) { b->onCommand(l); });
  b->input.start(inputSocket);
  b->start();
  return true;
}

bool bridgeConnected() { return gBridge != nullptr; }

void bridgeSetPrimaryDisplaySize(uint32_t width, uint32_t height) {
  if (gBridge) gBridge->input.setDisplaySize(width, height);
}

std::unique_ptr<Presenter> createBridgePresenter(
  int64_t displayId, uint32_t width, uint32_t height,
  std::unique_ptr<HeadlessPresenter> secondary
) {
  if (!gBridge) return nullptr;
  Spec s;
  s.id = int32_t(displayId);
  s.width = width;
  s.height = height;
  s.appId = "linuxapk";
  s.title = "Linux.apk";
  return addWindow(s, -1, std::move(secondary));
}

std::unique_ptr<Presenter> createBridgeVirtualPresenter(
  int64_t hwcDisplayId, uint32_t width, uint32_t height,
  std::unique_ptr<HeadlessPresenter> secondary
) {
  if (!gBridge) return nullptr;
  std::optional<Spec> spec = gBridge->claimPending(width, height);
  if (!spec) {
    LOGI(
      "virtual display %lld (%ux%u): no window requested; headless",
      (long long)hwcDisplayId, width, height
    );
    return nullptr;
  }
  return addWindow(*spec, hwcDisplayId, std::move(secondary));
}

}  // namespace linuxapk::hwc
