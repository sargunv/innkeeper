// SPDX-License-Identifier: AGPL-3.0-or-later
#define LOG_TAG "linuxapk-hwc"
#include <linux/dma-buf.h>
#include <sys/ioctl.h>

#include "Display.h"

#include "GpuCopy.h"
#include "Properties.h"
#include "linuxapk/log.h"

namespace linuxapk::hwc {

Display::Display(
  int64_t id, DisplayConfig config, std::unique_ptr<Presenter> presenter,
  bool isVirtual
)
    : mId(id),
      mVirtual(isVirtual),
      mConfig(config),
      mPresenter(std::move(presenter)),
      mVsync(id, config.vsyncPeriodNs) {
  LOGI(
    "display %lld%s: %dx%d @ %.2f Hz, %d dpi, presenter=%s", (long long)id,
    isVirtual ? " (virtual)" : "", config.width, config.height,
    1e9 / config.vsyncPeriodNs, config.dpi, mPresenter->name()
  );
  // SurfaceFlinger never sets a power mode on virtual displays.
  if (isVirtual) mPower = c3::PowerMode::ON;
}

int64_t Display::createLayer() {
  const int64_t id = mNextLayerId++;
  mLayers.emplace(id, Layer{});
  invalidate();
  return id;
}

Error Display::destroyLayer(int64_t id) {
  if (!mLayers.erase(id)) return Error::BadLayer;
  invalidate();
  return Error::None;
}

Layer* Display::layer(int64_t id) {
  auto it = mLayers.find(id);
  return it == mLayers.end() ? nullptr : &it->second;
}

Error Display::setClientTarget(const c3::Buffer& buffer) {
  BufferPtr b;
  Error e = mClientTargetSlots.update(buffer, &b);
  if (e != Error::None) return e;
  mClientTarget.buffer = std::move(b);
  mClientTargetUpdated = true;
  mClientTarget.acquireFence.reset(
    buffer.fence.get() >= 0 ? dupFd(buffer.fence.get()) : -1
  );
  return Error::None;
}

Error Display::setOutputBuffer(const c3::Buffer& buffer) {
  // GPU-only composition already rendered the sink. Mixed composition uses
  // scratch storage, which we blit into this buffer after its fence signals.
  BufferPtr b;
  Error e = mOutputSlots.update(buffer, &b);
  if (e == Error::None) {
    mOutputBuffer = std::move(b);
    mOutputAcquireFence.reset(
      buffer.fence.get() >= 0 ? dupFd(buffer.fence.get()) : -1
    );
  }
  return e;
}

Error Display::setPowerMode(c3::PowerMode mode) {
  switch (mode) {
    case c3::PowerMode::ON:
    case c3::PowerMode::OFF:
      break;
    default:
      return Error::Unsupported;  // no DOZE/SUSPEND capability advertised
  }
  if (mode != mPower) {
    LOGI(
      "display %lld: power %s", (long long)mId,
      mode == c3::PowerMode::ON ? "ON" : "OFF"
    );
  }
  mPower = mode;
  return Error::None;
}

void Display::setColorTransform(const std::vector<float>&) {
  // Not applied by us; every layer is CLIENT, so RenderEngine applies it.
  invalidate();
}

Error Display::validate(std::optional<c3::ChangedCompositionTypes>* changes) {
  if (mVirtual && getProperty("vendor.linuxapk.hwc.bench") == "1")
    LOGI(
      "BENCH validate display=%lld ns=%lld", (long long)mId, (long long)nowNs()
    );
  mPendingChanges.clear();
  for (auto& [id, l] : mLayers) {
    if (
      l.requested != c3::Composition::CLIENT &&
      l.requested != c3::Composition::CURSOR &&
      l.requested != c3::Composition::INVALID
    )
      mPendingChanges.push_back(id);
  }
  changes->reset();
  if (!mPendingChanges.empty()) {
    c3::ChangedCompositionTypes c;
    c.display = mId;
    for (int64_t id : mPendingChanges) {
      c3::ChangedCompositionLayer cl;
      cl.layer = id;
      cl.composition = c3::Composition::CLIENT;
      c.layers.push_back(cl);
    }
    *changes = std::move(c);
    mState = State::NeedAccept;
  } else {
    mState = State::ReadyToPresent;
  }
  return Error::None;
}

Error Display::acceptChanges() {
  if (mState != State::NeedAccept) {
    return mState == State::NeedValidate ? Error::NotValidated : Error::None;
  }
  for (int64_t id : mPendingChanges) {
    if (Layer* l = layer(id)) l->requested = c3::Composition::CLIENT;
  }
  mPendingChanges.clear();
  mState = State::ReadyToPresent;
  return Error::None;
}

Error Display::present() {
  if (mState == State::NeedValidate) return Error::NotValidated;
  if (mState == State::NeedAccept)
    acceptChanges();  // SF may present right after validate
  mState = State::NeedValidate;
  const bool bench =
    mVirtual && getProperty("vendor.linuxapk.hwc.bench") == "1";
  timespec cpuStart{};
  if (bench) clock_gettime(CLOCK_THREAD_CPUTIME_ID, &cpuStart);
  const int64_t begin = bench ? nowNs() : 0;
  CursorState* cursor = nullptr;
  for (auto& [id, l] : mLayers) {
    if (
      l.requested == c3::Composition::CURSOR && l.cursor.alpha > 0 &&
      l.cursor.buffer
    )
      cursor = &l.cursor;
  }
  ClientTargetState sink;
  ClientTargetState* target = &mClientTarget;
  bool sinkReady = mVirtual && mClientTargetUpdated && mOutputBuffer &&
                   mOutputBuffer->info() && mClientTarget.buffer &&
                   mClientTarget.buffer->info();
  if (
    sinkReady && mClientTarget.buffer->info()->bufferId.get() !=
                   mOutputBuffer->info()->bufferId.get()
  ) {
    sinkReady = waitFence(mClientTarget.acquireFence.get(), 1000) &&
                waitFence(mOutputAcquireFence.get(), 1000) &&
                mGpuCopy.copy(*mClientTarget.buffer, *mOutputBuffer);
    if (sinkReady) {
      sink.buffer = mOutputBuffer;
      target = &sink;
    }
  }
  if (mPower == c3::PowerMode::ON)
    mPresenter->present(
      {mId, target, mOutputBuffer, cursor,
       mVirtual ? sinkReady : mClientTargetUpdated}
    );
  mPresentFence.reset();
  if (sinkReady) {
    dma_buf_export_sync_file fence{.flags = DMA_BUF_SYNC_READ, .fd = -1};
    if (
      ioctl(
        mOutputBuffer->info()->fd, DMA_BUF_IOCTL_EXPORT_SYNC_FILE, &fence
      ) == 0
    )
      mPresentFence.reset(fence.fd);
  }
  mClientTargetUpdated = false;
  if (bench) {
    timespec cpuEnd{};
    clock_gettime(CLOCK_THREAD_CPUTIME_ID, &cpuEnd);
    LOGI(
      "BENCH hwc display=%lld begin_ns=%lld end_ns=%lld cpu_ns=%lld",
      (long long)mId, (long long)begin, (long long)nowNs(),
      (long long)((cpuEnd.tv_sec - cpuStart.tv_sec) * 1000000000LL +
                  cpuEnd.tv_nsec - cpuStart.tv_nsec)
    );
  }
  return Error::None;
}

}  // namespace linuxapk::hwc
