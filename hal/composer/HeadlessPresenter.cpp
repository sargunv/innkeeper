// SPDX-License-Identifier: AGPL-3.0-or-later
#define LOG_TAG "linuxapk-hwc"
#include <condition_variable>
#include <mutex>
#include <thread>
#include <vector>

#include <pthread.h>
#include <string.h>

#include "FrameDumper.h"
#include "Presenter.h"
#include "Properties.h"
#ifdef LINUXAPK_HWC_WAYLAND
#include "BridgePresenter.h"
#include "WaylandPresenter.h"
#endif
#include "linuxapk/log.h"

namespace linuxapk::hwc {

// Encodes dumps off the present path: present() copies the pixels (a few ms)
// and the worker compresses/writes them. Frames are dropped while it is busy.
struct HeadlessPresenter::Worker {
  FrameDumper* dumper;
  std::mutex mutex;
  std::condition_variable cv;
  bool stop = false;
  bool pending = false;
  std::string prefix;
  std::vector<uint8_t> pixels;
  FrameView view{};
  std::thread thread;

  explicit Worker(FrameDumper* d) : dumper(d) {
    thread = std::thread([this] {
      pthread_setname_np(pthread_self(), "hwc-dump");
      std::unique_lock lock(mutex);
      while (true) {
        cv.wait(lock, [this] { return stop || pending; });
        if (stop) return;
        FrameView v = view;
        std::string p = prefix;
        lock.unlock();
        dumper->dump(p, v);
        lock.lock();
        pending = false;
      }
    });
  }
  ~Worker() {
    {
      std::lock_guard lock(mutex);
      stop = true;
    }
    cv.notify_all();
    thread.join();
  }
  bool submit(const std::string& pfx, const FrameView& src) {
    std::lock_guard lock(mutex);
    if (pending) return false;
    const size_t rowBytes = src.strideBytes;
    pixels.resize(rowBytes * src.height);
    memcpy(pixels.data(), src.data, pixels.size());
    view = src;
    view.data = pixels.data();
    prefix = pfx;
    pending = true;
    cv.notify_all();
    return true;
  }
};

HeadlessPresenter::HeadlessPresenter(std::unique_ptr<FrameDumper> dumper)
    : mDumper(std::move(dumper)) {}

HeadlessPresenter::~HeadlessPresenter() = default;

Error HeadlessPresenter::present(FrameInput& frame, PresentResult* out) {
  mPresented++;
  const int64_t now = nowNs();
  if (now - mLastLogNs > 60'000'000'000) {
    LOGI(
      "display %lld: %llu frames presented (headless)",
      (long long)frame.displayId, (unsigned long long)mPresented
    );
    mLastLogNs = now;
  }
  ClientTargetState* ct = frame.clientTarget;
  mDumper->reloadFromProperties(now);
  if (!mWorker && mDumper->enabled())
    mWorker = std::make_unique<Worker>(mDumper.get());
  if (mWorker && ct && ct->buffer && mDumper->shouldDump(now)) {
    const BufferHandle* bh = ct->buffer->info();
    const uint8_t* base = ct->buffer->map();
    if (bh && base) {
      // The client target is complete once its acquire fence signals.
      if (!waitFence(ct->acquireFence.get(), 1000)) {
        LOGW("client target fence did not signal; dumping anyway");
      }
      FrameView v{
        base + bh->planes[0].offset.get(), bh->width, bh->height,
        bh->planes[0].stride, bh->resolvedFormat
      };
      char prefix[32];
      snprintf(
        prefix, sizeof prefix, "display%lld", (long long)frame.displayId
      );
      mWorker->submit(mPrefix.empty() ? prefix : mPrefix, v);
    }
  }
  // Nothing holds the buffers after we return: no present/release fences.
  out->presentFence.reset();
  return Error::None;
}

bool singleWindowMode() {
  return getProperty("vendor.linuxapk.wm.mode", "per-app") == "single";
}

static std::unique_ptr<HeadlessPresenter> makeHeadless() {
  return std::make_unique<HeadlessPresenter>(
    std::make_unique<FrameDumper>(FrameDumper::configFromProperties())
  );
}

std::unique_ptr<Presenter> createPresenter(
  int64_t displayId, uint32_t width, uint32_t height
) {
  std::string backend = getProperty("vendor.linuxapk.hwc.backend", "headless");
#ifdef LINUXAPK_HWC_WAYLAND
  if (backend == "bridge" || backend == "wayland") {
    const bool bridge = bridgeStarted();
    if (bridge) {
      bridgeSetPrimaryDisplaySize(width, height);
    } else {
      setPrimaryDisplaySize(width, height);
    }
    if (!singleWindowMode()) {
      LOGI(
        "display %lld: per-app windows (vendor.linuxapk.wm.mode), no window",
        (long long)displayId
      );
      return makeHeadless();
    }
    auto p =
      bridge ? createBridgePresenter(displayId, width, height, makeHeadless())
             : createWaylandPresenter(displayId, width, height, makeHeadless());
    if (p) return p;
    LOGW("no window backend available; using headless");
    return makeHeadless();
  }
#endif
  if (backend != "headless")
    LOGW("unknown backend '%s'; using headless", backend.c_str());
  return makeHeadless();
}

std::unique_ptr<Presenter> createVirtualPresenter(
  int64_t displayId, uint32_t width, uint32_t height
) {
#ifdef LINUXAPK_HWC_WAYLAND
  if (bridgeStarted()) {
    if (
      auto p =
        createBridgeVirtualPresenter(displayId, width, height, makeHeadless())
    ) {
      return p;
    }
  } else if (WaylandConnection::get()) {
    if (
      auto p =
        createWaylandVirtualPresenter(displayId, width, height, makeHeadless())
    ) {
      return p;
    }
  }
#endif
  return makeHeadless();
}

}  // namespace linuxapk::hwc
