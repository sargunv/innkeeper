// SPDX-License-Identifier: AGPL-3.0-or-later
#define LOG_TAG "linuxapk-hwc"
#include "Display.h"

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
  mClientTarget.acquireFence.reset(
    buffer.fence.get() >= 0 ? dupFd(buffer.fence.get()) : -1
  );
  return Error::None;
}

Error Display::setOutputBuffer(const c3::Buffer& buffer) {
  // The release fence only matters to a composer that writes the output
  // buffer. Every layer is CLIENT, so SurfaceFlinger renders into this very
  // buffer (it is also the client target) and we only read it.
  BufferPtr b;
  Error e = mOutputSlots.update(buffer, &b);
  if (e == Error::None) mOutputBuffer = std::move(b);
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
  mPendingChanges.clear();
  for (auto& [id, l] : mLayers) {
    if (
      l.requested != c3::Composition::CLIENT &&
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
  if (mPower == c3::PowerMode::ON)
    mPresenter->present({mId, &mClientTarget, mOutputBuffer});
  return Error::None;
}

}  // namespace linuxapk::hwc
