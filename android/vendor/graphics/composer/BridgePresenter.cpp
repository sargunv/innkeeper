// SPDX-License-Identifier: AGPL-3.0-or-later
#define LOG_TAG "linuxapk-hwc"
#include <algorithm>
#include <atomic>
#include <initializer_list>
#include <map>
#include <mutex>
#include <thread>
#include <vector>

#include <errno.h>
#include <fcntl.h>
#include <linux/udmabuf.h>
#include <pthread.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>

#include "BridgePresenter.h"

#include "linuxapk/bridge_client.h"
#include "linuxapk/layout.h"
#include "linuxapk/log.h"

namespace linuxapk::hwc {

namespace {

// The display service's messages (host/bridge/src/display/proto.rs).
constexpr uint16_t kDisplayAdd = 0x0201;
constexpr uint16_t kDisplayRemove = 0x0202;
constexpr uint16_t kBufferAdd = 0x0203;
constexpr uint16_t kBufferRemove = 0x0204;
constexpr uint16_t kFrame = 0x0205;
constexpr uint16_t kEvRelease = 0x4201;
constexpr int kPoolSize = 3;
constexpr uint32_t kXBGR8888 = fourcc('X', 'B', '2', '4');  // R,G,B,X bytes
constexpr uint32_t kXRGB8888 = fourcc('X', 'R', '2', '4');  // B,G,R,X bytes

lxb_msg message(
  uint16_t type, uint32_t tag, std::initializer_list<uint32_t> fields
) {
  lxb_msg m;
  lxb_msg_init(&m, type, tag);
  for (uint32_t v : fields) lxb_put_u32(&m, v);
  return m;
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
  void cursor(const CursorState* cursor);
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
    mCursorKnown = false;
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
  std::vector<uint8_t> mCursor;
  int mCursorX = 0, mCursorY = 0;
  bool mCursorKnown = false;
};

class Bridge {
 public:
  std::mutex mutex;  // displays, their window and pool state
  std::atomic<uint32_t> nextBuffer{1};

  bool send(const lxb_msg& m, int fd = -1);
  uint32_t tag() { return mTag.fetch_add(1) + 1; }
  // Connects in the background, and again whenever the connection drops.
  void start() {
    mThread = std::thread([this] { loop(); });
  }
  bool connected() const { return mSock.load() >= 0; }
  // Registers a display; tells the bridge now if connected, else on
  // connect.
  void addDisplay(std::shared_ptr<BridgeDisplay> d);
  void removeDisplay(uint32_t id);

 private:
  void loop();
  void handle(const lxb_packet& p);
  void sendAdd(const BridgeDisplay& d);
  std::atomic<int> mSock{-1};
  std::atomic<uint32_t> mTag{1};
  std::map<uint32_t, std::shared_ptr<BridgeDisplay>> mDisplays;
  std::map<uint32_t, uint32_t> mAdds;  // tag of DISPLAY_ADD -> display
  std::thread mThread;
};

static Bridge* gBridge = nullptr;

// ---- connection
// ------------------------------------------------------------------

bool Bridge::send(const lxb_msg& m, int fd) {
  const int s = mSock.load();
  if (s < 0) return false;
  if (const int r = lxb_send(s, &m, fd); r != 0) {
    LOGW("bridge: send: %s", strerror(-r));
    return false;
  }
  return true;
}

// Bridge lock held.
void Bridge::sendAdd(const BridgeDisplay& d) {
  const uint32_t t = tag();
  mAdds[t] = d.id();
  if (!send(message(kDisplayAdd, t, {d.id(), d.width(), d.height()})))
    mAdds.erase(t);
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
  send(message(kDisplayRemove, tag(), {id}));
}

void Bridge::handle(const lxb_packet& p) {
  if (p.type == LXB_REPLY) {
    std::lock_guard lock(mutex);
    auto add = mAdds.find(p.tag);
    if (add == mAdds.end()) {
      // -ENOENT: a display or buffer the bridge already dropped.
      if (p.status != 0 && p.status != -ENOENT)
        LOGW("bridge: request %u failed: %d", p.tag, p.status);
      return;
    }
    const uint32_t id = add->second;
    mAdds.erase(add);
    auto it = mDisplays.find(id);
    if (it == mDisplays.end()) return;
    if (p.status == 0 && p.len >= 4) {
      const uint32_t window = lxb_u32(p.body);
      it->second->setWindow(window);
      LOGI("virtual display %u: shown in window %u", id, window);
    } else if (p.status == -ENOENT) {
      LOGI("virtual display %u: no window asked for; headless", id);
    } else {
      LOGW("virtual display %u: the bridge refused it: %d", id, p.status);
    }
    return;
  }
  if (p.type == kEvRelease && p.len >= 4) {
    const uint32_t buffer = lxb_u32(p.body);
    std::lock_guard lock(mutex);
    for (auto& [id, d] : mDisplays) d->onRelease(buffer);
  }
}

// "Not connected yet" and "connection lost" are the same state: the bridge
// may start before or after the composer, and may restart.
void Bridge::loop() {
  pthread_setname_np(pthread_self(), "hwc-bridge");
  lxb_backoff backoff{};
  int lastError = 0;
  while (true) {
    if (const int s = mSock.load(); s >= 0) {
      lxb_packet p;
      const int r = lxb_recv(s, &p);
      if (r == 0) {
        handle(p);
        lxb_close_fds(&p);
        continue;
      }
      LOGW("bridge: connection lost (%s); reconnecting", strerror(-r));
      mSock = -1;
      close(s);
    }
    if (const int ms = lxb_backoff_ms(&backoff); ms > 0) {
      usleep(useconds_t(ms) * 1000);
      continue;
    }
    const int fd = lxb_connect(LXB_SERVICE_DISPLAY, "composer");
    if (fd < 0) {
      if (fd != lastError)
        LOGW(
          "bridge: connect %s: %s; retrying every second", LXB_SOCKET_PATH,
          strerror(-fd)
        );
      lastError = fd;
      continue;
    }
    lastError = 0;
    LOGI("bridge: display service on %s", LXB_SOCKET_PATH);
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
    if (tellBridge) b->send(message(kBufferRemove, b->tag(), {buf.id}));
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
      lxb_msg m = message(
        kBufferAdd, b->tag(), {buf.id, mId, w, h, format, pool.stride, 0}
      );
      lxb_put_u64(&m, 0);  // modifier: LINEAR
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

// CURSOR is accepted by validate rather than converted to CLIENT: these
// pixels are exclusively the Wayland cursor, never part of the window frame.
void BridgeDisplay::cursor(const CursorState* cursor) {
  std::vector<uint8_t> pixels;
  uint32_t w = 0, h = 0;
  int x = 0, y = 0;
  if (cursor && cursor->buffer) {
    const auto* bh = cursor->buffer->info();
    if (
      !bh || bh->width > 512 || bh->height > 512 ||
      (bh->resolvedFormat != fmt::RGBA_8888 &&
       bh->resolvedFormat != fmt::BGRA_8888)
    )
      return;
    if (!waitFence(cursor->acquireFence.get(), 1000)) return;
    const auto* map = cursor->buffer->map();
    if (!map) return;
    w = bh->width;
    h = bh->height;
    x = cursor->x;
    y = cursor->y;
    pixels.resize(size_t(w) * h * 4);
    for (uint32_t row = 0; row < h; row++) {
      auto* dst = pixels.data() + size_t(row) * w * 4;
      const auto* src =
        map + bh->planes[0].offset.get() + size_t(row) * bh->planes[0].stride;
      for (uint32_t col = 0; col < w; col++) {
        // wl_shm ARGB8888 is B,G,R,A in memory, premultiplied like Android.
        dst[col * 4] =
          src[col * 4 + (bh->resolvedFormat == fmt::RGBA_8888 ? 2 : 0)];
        dst[col * 4 + 1] = src[col * 4 + 1];
        dst[col * 4 + 2] =
          src[col * 4 + (bh->resolvedFormat == fmt::RGBA_8888 ? 0 : 2)];
        dst[col * 4 + 3] = src[col * 4 + 3];
        for (int channel = 0; channel < 4; ++channel)
          dst[col * 4 + channel] = uint8_t(
            dst[col * 4 + channel] * std::clamp(cursor->alpha, 0.f, 1.f)
          );
      }
    }
  }
  if (mCursorKnown && pixels == mCursor && x == mCursorX && y == mCursorY)
    return;
  int fd = -1;
  if (!pixels.empty()) {
    fd = memfd_create("linuxapk-cursor", MFD_CLOEXEC | MFD_ALLOW_SEALING);
    if (fd < 0) return;
    if (
      write(fd, pixels.data(), pixels.size()) != ssize_t(pixels.size()) ||
      fcntl(
        fd, F_ADD_SEALS,
        F_SEAL_WRITE | F_SEAL_GROW | F_SEAL_SHRINK | F_SEAL_SEAL
      ) != 0
    ) {
      close(fd);
      return;
    }
  }
  bool sent = b->send(
    message(0x0206, b->tag(), {mId, w, h, uint32_t(x), uint32_t(y)}), fd
  );
  if (fd >= 0) close(fd);
  if (sent) {
    mCursor = std::move(pixels);
    mCursorX = x;
    mCursorY = y;
    mCursorKnown = true;
  }
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
  cursor(frame.cursor);
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
  if (!b->send(message(kFrame, b->tag(), {mId, id, 0, 0, 0, 0}))) {
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
  gBridge->start();
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
