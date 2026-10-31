// SPDX-License-Identifier: AGPL-3.0-or-later
#define LOG_TAG "innkeeper-hwc"
#include <condition_variable>
#include <mutex>
#include <thread>
#include <vector>

#include <pthread.h>
#include <string.h>

#include "FrameDumper.h"
#include "GpuCopy.h"
#include "Presenter.h"
#include "innkeeper/buffer_alloc.h"
#include "innkeeper/log.h"

namespace innkeeper::hwc {

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

void HeadlessPresenter::present(const FrameInput& frame) {
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
  mDumper->reloadFromProperty(now);
  if (!mWorker && mDumper->enabled())
    mWorker = std::make_unique<Worker>(mDumper.get());
  if (mWorker && ct && ct->buffer && mDumper->shouldDump(now)) {
    BufferPtr pixels = ct->buffer;
    const BufferHandle* bh = pixels->info();
    if (!bh) return;
    // The client target is complete once its acquire fence signals.
    if (!waitFence(ct->acquireFence.get(), 1000))
      LOGW("client target fence did not signal; dumping anyway");
    if (bh->flags & kFlagTiled) {
      // PNG encoding is a CPU consumer. Read tiled targets through the GPU,
      // rather than interpreting their physical tile bytes as scanlines.
      const auto* previous = mReadback ? mReadback->info() : nullptr;
      if (
        !previous || previous->width != bh->width ||
        previous->height != bh->height ||
        previous->resolvedFormat != bh->resolvedFormat
      ) {
        native_handle_t* handle = nullptr;
        if (
          allocateBuffer(
            {"frame-dump", bh->width, bh->height, 1, bh->resolvedFormat,
             usage::CPU_READ_MASK | usage::GPU_RENDER_TARGET |
               usage::GPU_TEXTURE,
             0},
            &handle
          ) != AllocStatus::Ok
        )
          return;
        mReadback = std::make_shared<BufferRef>(handle);
      }
      if (!mReadbackCopy) mReadbackCopy = std::make_unique<GpuCopy>();
      if (!mReadbackCopy->copy(*pixels, *mReadback)) return;
      pixels = mReadback;
      bh = pixels->info();
    }
    const uint8_t* base = pixels->map();
    if (base) {
      FrameView v{
        base + bh->planes[0].offset.get(), bh->width, bh->height,
        bh->planes[0].stride, bh->resolvedFormat
      };
      char prefix[32];
      snprintf(
        prefix, sizeof prefix, "display%lld", (long long)frame.displayId
      );
      mWorker->submit(prefix, v);
    }
  }
}

}  // namespace innkeeper::hwc
