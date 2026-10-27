// SPDX-License-Identifier: AGPL-3.0-or-later
#define LOG_TAG "linuxapk-hwc"
#include <algorithm>

#include "Display.h"

#include "Properties.h"
#include "linuxapk/log.h"

namespace linuxapk::hwc {

DisplayConfig DisplayConfig::fromProperties() {
  DisplayConfig c;
  c.width = int32_t(getIntProperty("vendor.linuxapk.display.width", c.width));
  c.height =
    int32_t(getIntProperty("vendor.linuxapk.display.height", c.height));
  int64_t hz = getIntProperty("vendor.linuxapk.display.refresh_hz", 60);
  if (hz < 1 || hz > 1000) hz = 60;
  c.vsyncPeriodNs = int32_t(1'000'000'000 / hz);
  c.dpi = int32_t(getIntProperty("vendor.linuxapk.display.dpi", c.dpi));
  if (c.width < 1 || c.height < 1 || c.width > 16384 || c.height > 16384) {
    LOGW("bad display size %dx%d; using 1280x800", c.width, c.height);
    c.width = 1280;
    c.height = 800;
  }
  return c;
}

Display::Display(
  int64_t id, DisplayConfig config, std::unique_ptr<Presenter> presenter
)
    : mId(id),
      mConfig(config),
      mPresenter(std::move(presenter)),
      mVsync(id, config.vsyncPeriodNs) {
  LOGI(
    "display %lld: %dx%d @ %.2f Hz, %d dpi, presenter=%s", (long long)id,
    config.width, config.height, 1e9 / config.vsyncPeriodNs, config.dpi,
    mPresenter->name()
  );
}

int64_t Display::createLayer() {
  int64_t id = mNextLayerId++;
  auto l = std::make_unique<Layer>();
  l->id = id;
  mLayers.emplace(id, std::move(l));
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
  return it == mLayers.end() ? nullptr : it->second.get();
}

Error Display::setClientTarget(
  const c3::Buffer& buffer, common::Dataspace dataspace
) {
  BufferPtr b;
  Error e = mClientTargetSlots.update(buffer, &b);
  if (e != Error::None) return e;
  mClientTarget.buffer = std::move(b);
  mClientTarget.acquireFence.reset(
    buffer.fence.get() >= 0 ? dupFd(buffer.fence.get()) : -1
  );
  mClientTarget.dataspace = dataspace;
  return Error::None;
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
    mPresenter->setPowerOn(mode == c3::PowerMode::ON);
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
    c3::Composition want = l->requested;
    if (want == c3::Composition::INVALID) want = c3::Composition::CLIENT;
    c3::Composition got = c3::Composition::CLIENT;
    if (want != c3::Composition::CLIENT && mPresenter->acceptDeviceLayer(*l))
      got = want;
    if (got != want) mPendingChanges.emplace_back(id, got);
    l->composition = got;
  }
  changes->reset();
  if (!mPendingChanges.empty()) {
    c3::ChangedCompositionTypes c;
    c.display = mId;
    for (auto& [id, comp] : mPendingChanges) {
      c3::ChangedCompositionLayer cl;
      cl.layer = id;
      cl.composition = comp;
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
  for (auto& [id, comp] : mPendingChanges) {
    if (Layer* l = layer(id)) l->requested = comp;
  }
  mPendingChanges.clear();
  mState = State::ReadyToPresent;
  return Error::None;
}

Error Display::present(PresentResult* out) {
  if (mState == State::NeedValidate) return Error::NotValidated;
  if (mState == State::NeedAccept)
    acceptChanges();  // SF may present right after validate
  mState = State::NeedValidate;

  FrameInput f{};
  f.displayId = mId;
  f.width = uint32_t(mConfig.width);
  f.height = uint32_t(mConfig.height);
  f.frameNumber = ++mFrameNumber;
  f.clientTarget = &mClientTarget;
  for (auto& [_, l] : mLayers) {
    f.layers.push_back(l.get());
    if (l->composition == c3::Composition::CLIENT) f.hasClientLayers = true;
  }
  std::stable_sort(
    f.layers.begin(), f.layers.end(),
    [](const LayerState* a, const LayerState* b) { return a->z < b->z; }
  );
  if (mPower != c3::PowerMode::ON) return Error::None;
  return mPresenter->present(f, out);
}

}  // namespace linuxapk::hwc
