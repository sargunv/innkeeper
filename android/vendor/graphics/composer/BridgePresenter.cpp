// SPDX-License-Identifier: AGPL-3.0-or-later
#define LOG_TAG "linuxapk-hwc"
#include <algorithm>
#include <atomic>
#include <map>
#include <mutex>
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

#include "linuxapk/layout.h"
#include "linuxapk/log.h"

namespace linuxapk::hwc {

namespace {

// The bridge protocol's framing and the display service (id 2).
constexpr uint16_t kHello = 0x0001;
constexpr uint16_t kReply = 0x8000;
constexpr uint32_t kServiceDisplay = 2;
constexpr uint16_t kDisplayAdd = 0x0201;
constexpr uint16_t kDisplayRemove = 0x0202;
constexpr uint16_t kBufferAdd = 0x0203;
constexpr uint16_t kBufferRemove = 0x0204;
constexpr uint16_t kFrame = 0x0205;
constexpr uint16_t kEvRelease = 0x4201;
constexpr int32_t kENOENT = 2;
constexpr size_t kMaxMsg = 512;
constexpr int kPoolSize = 3;
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
  Msg& name(const char* s) {
    const size_t n = strlen(s);
    u16(uint16_t(n));
    b.insert(b.end(), s, s + n);
    return *this;
  }
};

uint32_t rd32(const uint8_t* p) {
  return uint32_t(p[0]) | uint32_t(p[1]) << 8 | uint32_t(p[2]) << 16 |
         uint32_t(p[3]) << 24;
}

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

// One HWC virtual display, as the bridge sees it.
class BridgeDisplay {
 public:
  BridgeDisplay(Bridge* b, uint32_t id, uint32_t width, uint32_t height)
      : b(b), mId(id), mWidth(width), mHeight(height) {}
  ~BridgeDisplay();
  uint32_t id() const { return mId; }
  uint32_t width() const { return mWidth; }
  uint32_t height() const { return mHeight; }
  void present(const FrameInput& frame);
  // The rest: bridge lock held.
  void onRelease(uint32_t buffer);
  // DISPLAY_ADD's answer: shown in `window`, or not shown (-1).
  void setWindow(int64_t window) { mWindow = window; }
  bool shown() const { return mWindow >= 0; }
  // The bridge (re)connected without our buffers: not shown until it says
  // so again, and the next frame adds a new pool.
  void reset() {
    mWindow = -1;
    mPoolStale = true;
  }

 private:
  bool makePool(uint32_t w, uint32_t h, uint32_t format, Pool* out);
  void freePool(Pool& p, bool tellBridge);
  Bridge* b;
  const uint32_t mId;
  const uint32_t mWidth, mHeight;
  int64_t mWindow = -1;  // guarded by the bridge lock
  Pool mPool;  // written by present(); busy flags under the bridge lock
  bool mPoolStale = false;  // guarded by the bridge lock
  int mLast = -1;
  uint64_t mFrames = 0, mDropped = 0;
  int64_t mLastLogNs = 0;
  bool mWarnedFormat = false;
};

class Bridge {
 public:
  std::mutex mutex;  // displays, their window and pool state
  std::atomic<uint32_t> nextBuffer{1};

  bool send(Msg& m, int fd = -1);
  uint32_t tag() { return mTag.fetch_add(1) + 1; }
  // Connects in the background, and again whenever the connection drops.
  void start(std::string path) {
    mPath = std::move(path);
    mThread = std::thread([this] { loop(); });
  }
  bool connected() const { return mSock.load() >= 0; }
  // Registers a display; tells the bridge now if connected, else on
  // connect.
  void addDisplay(std::shared_ptr<BridgeDisplay> d);
  void removeDisplay(uint32_t id);

 private:
  void loop();
  int connect(std::string* error);
  void handle(const uint8_t* m, size_t n);
  void sendAdd(const BridgeDisplay& d);
  std::string mPath;
  std::atomic<int> mSock{-1};
  std::mutex mSendMutex;
  std::atomic<uint32_t> mTag{1};
  std::map<uint32_t, std::shared_ptr<BridgeDisplay>> mDisplays;
  std::map<uint32_t, uint32_t> mAdds;  // tag of DISPLAY_ADD -> display
  std::thread mThread;
};

static Bridge* gBridge = nullptr;

// ---- connection
// ------------------------------------------------------------------

// Returns the socket after the display service accepted our HELLO, or -1
// with the reason in *error.
int Bridge::connect(std::string* error) {
  int fd = socket(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0);
  sockaddr_un addr{};
  addr.sun_family = AF_UNIX;
  if (fd < 0 || mPath.size() >= sizeof(addr.sun_path)) {
    *error = "socket: " + std::string(strerror(fd < 0 ? errno : ENAMETOOLONG));
    if (fd >= 0) close(fd);
    return -1;
  }
  memcpy(addr.sun_path, mPath.c_str(), mPath.size() + 1);
  if (::connect(fd, reinterpret_cast<sockaddr*>(&addr), sizeof addr) != 0) {
    *error = "connect " + mPath + ": " + strerror(errno);
    close(fd);
    return -1;
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
    *error = "the display service refused us (" +
             (n < 12 ? std::string("no reply")
                     : std::to_string(int32_t(rd32(buf + 8)))) +
             ")";
    close(fd);
    return -1;
  }
  return fd;
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

// Bridge lock held.
void Bridge::sendAdd(const BridgeDisplay& d) {
  const uint32_t t = tag();
  Msg m(kDisplayAdd, t);
  m.u32(d.id()).u32(d.width()).u32(d.height());
  mAdds[t] = d.id();
  if (!send(m)) mAdds.erase(t);
}

void Bridge::addDisplay(std::shared_ptr<BridgeDisplay> d) {
  std::lock_guard lock(mutex);
  // Same lock as loop()'s connect: exactly one of us adds the display.
  if (connected()) sendAdd(*d);
  mDisplays[d->id()] = std::move(d);
}

void Bridge::removeDisplay(uint32_t id) {
  std::shared_ptr<BridgeDisplay> gone;
  {
    std::lock_guard lock(mutex);
    auto it = mDisplays.find(id);
    if (it == mDisplays.end()) return;
    gone = std::move(it->second);
    mDisplays.erase(it);
  }
  // Even if not shown yet: the bridge may be showing it by now. It drops
  // the display's buffers with it.
  Msg m(kDisplayRemove, tag());
  m.u32(id);
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
    std::lock_guard lock(mutex);
    auto add = mAdds.find(t);
    if (add == mAdds.end()) {
      // -ENOENT: a display or buffer the bridge already dropped.
      if (status != 0 && status != -kENOENT)
        LOGW("bridge: request %u failed: %d", t, status);
      return;
    }
    const uint32_t id = add->second;
    mAdds.erase(add);
    auto it = mDisplays.find(id);
    if (it == mDisplays.end()) return;
    if (status == 0 && len >= 8) {
      const uint32_t window = rd32(body + 4);
      it->second->setWindow(window);
      LOGI("virtual display %u: shown in window %u", id, window);
    } else if (status == -kENOENT) {
      LOGI("virtual display %u: no window asked for; headless", id);
    } else {
      LOGW("virtual display %u: the bridge refused it: %d", id, status);
    }
    return;
  }
  if (type == kEvRelease && len >= 4) {
    const uint32_t buffer = rd32(body);
    std::lock_guard lock(mutex);
    for (auto& [id, d] : mDisplays) d->onRelease(buffer);
  }
}

// "Not connected yet" and "connection lost" are the same state: the bridge
// may start before or after the composer, and may restart.
void Bridge::loop() {
  pthread_setname_np(pthread_self(), "hwc-bridge");
  uint8_t buf[kMaxMsg + 1];
  std::string lastError;
  while (true) {
    if (const int s = mSock.load(); s >= 0) {
      ssize_t n = recv(s, buf, sizeof buf, 0);
      if (n > 0) {
        handle(buf, size_t(n));
        continue;
      }
      if (n < 0 && errno == EINTR) continue;
      LOGW("bridge: connection lost; reconnecting");
      mSock = -1;
      close(s);
      sleep(1);
    }
    std::string error;
    const int fd = connect(&error);
    if (fd < 0) {
      if (error != lastError)
        LOGW("bridge: %s; retrying every second", error.c_str());
      lastError = std::move(error);
      sleep(1);
      continue;
    }
    lastError.clear();
    LOGI("bridge: display service on %s", mPath.c_str());
    // The bridge has none of our displays or buffers: add them again.
    std::lock_guard lock(mutex);
    mSock = fd;
    mAdds.clear();
    for (auto& [id, d] : mDisplays) {
      d->reset();
      sendAdd(*d);
    }
  }
}

// ---- display
// ---------------------------------------------------------------------

BridgeDisplay::~BridgeDisplay() {
  std::lock_guard lock(b->mutex);
  freePool(mPool, /*tellBridge=*/false);
}

void BridgeDisplay::freePool(Pool& p, bool tellBridge) {
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

void BridgeDisplay::onRelease(uint32_t buffer) {
  for (auto& buf : mPool.bufs) {
    if (buf.id == buffer) buf.busy = false;
  }
}

bool BridgeDisplay::makePool(
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
        .u32(mId)
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

void BridgeDisplay::present(const FrameInput& frame) {
  {
    std::lock_guard lock(b->mutex);
    if (!shown()) return;
    if (mPoolStale) {
      freePool(mPool, /*tellBridge=*/false);
      mPoolStale = false;
      mLast = -1;
    }
  }
  ClientTargetState* ct = frame.clientTarget;
  BufferPtr buffer = ct && ct->buffer ? ct->buffer : frame.outputBuffer;
  if (!buffer) return;
  const BufferHandle* bh = buffer->info();
  const uint8_t* src = buffer->map();
  if (!bh || !src) return;
  const int32_t f = bh->resolvedFormat;
  uint32_t format;
  if (f == fmt::RGBA_8888 || f == fmt::RGBX_8888) {
    format = kXBGR8888;
  } else if (f == fmt::BGRA_8888) {
    format = kXRGB8888;
  } else {
    if (!mWarnedFormat) LOGW("bridge: frame format 0x%x not supported yet", f);
    mWarnedFormat = true;
    return;
  }
  if (ct && ct->buffer == buffer && !waitFence(ct->acquireFence.get(), 1000))
    LOGW("bridge: client target fence timeout");
  src += bh->planes[0].offset.get();
  const uint32_t srcStride = bh->planes[0].stride;
  const uint32_t w = bh->width, h = bh->height;
  if (w == 0 || h == 0 || !b->connected()) return;

  if (w != mPool.width || h != mPool.height || format != mPool.format) {
    Pool np;
    if (!makePool(w, h, format, &np)) {
      LOGE("bridge: display %u: no %ux%u buffers", mId, w, h);
      return;
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
      return;
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
  m.u32(mId).u32(id).u32(0).u32(0).u32(0).u32(0);
  if (!b->send(m)) {
    std::lock_guard lock(b->mutex);
    mPool.bufs[size_t(idx)].busy = false;
  }
  const int64_t now = nowNs();
  if (now - mLastLogNs > 60'000'000'000) {
    LOGI(
      "bridge: display %u: %llu frames, %llu dropped (all buffers busy), "
      "frame %ux%u",
      mId, (unsigned long long)mFrames, (unsigned long long)mDropped, w, h
    );
    mLastLogNs = now;
  }
}

// ---- presenters
// ------------------------------------------------------------------

namespace {
class BridgeVirtualPresenter : public Presenter {
 public:
  BridgeVirtualPresenter(
    std::shared_ptr<BridgeDisplay> display,
    std::unique_ptr<HeadlessPresenter> secondary
  )
      : mDisplay(std::move(display)), mSecondary(std::move(secondary)) {}
  ~BridgeVirtualPresenter() override { gBridge->removeDisplay(mDisplay->id()); }
  const char* name() const override { return "bridge"; }
  void present(const FrameInput& frame) override {
    mSecondary->present(frame);
    mDisplay->present(frame);
  }

 private:
  std::shared_ptr<BridgeDisplay> mDisplay;
  std::unique_ptr<HeadlessPresenter> mSecondary;
};
}  // namespace

void startBridge() {
  gBridge = new Bridge();
  gBridge->start("/dev/socket/linuxapk/bridge.sock");
}

bool bridgeStarted() { return gBridge != nullptr; }

std::unique_ptr<Presenter> createBridgeVirtualPresenter(
  int64_t hwcDisplayId, uint32_t width, uint32_t height,
  std::unique_ptr<HeadlessPresenter> secondary
) {
  auto d = std::make_shared<BridgeDisplay>(
    gBridge, uint32_t(hwcDisplayId), width, height
  );
  gBridge->addDisplay(d);
  return std::make_unique<BridgeVirtualPresenter>(d, std::move(secondary));
}

}  // namespace linuxapk::hwc
