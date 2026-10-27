// SPDX-License-Identifier: AGPL-3.0-or-later
#define LOG_TAG "linuxapk-hwc"
#include <android/binder_ibinder_platform.h>

#include "ComposerClient.h"

#include "Properties.h"
#include "linuxapk/log.h"

namespace linuxapk::hwc {

namespace {
constexpr int64_t kPrimaryDisplay = 0;
// HWC ids of virtual displays (SurfaceFlinger maps them to its own ids). Never
// reused, so a late command for a destroyed display can't hit a new one.
constexpr int64_t kFirstVirtualDisplay = 100;
constexpr int32_t kMaxVirtualDisplays = 32;
}  // namespace

class ComposerClient::Results {
 public:
  explicit Results(std::vector<CommandResultPayload>* out) : mOut(out) {}
  void setIndex(int32_t i) { mIndex = i; }
  void error(Error e) {
    if (e == Error::None) return;
    CommandError ce;
    ce.commandIndex = mIndex;
    ce.errorCode = int32_t(e);
    mOut->emplace_back(std::move(ce));
  }
  void add(CommandResultPayload p) { mOut->emplace_back(std::move(p)); }

 private:
  std::vector<CommandResultPayload>* mOut;
  int32_t mIndex = 0;
};

ComposerClient::ComposerClient() {
  mNextVirtualDisplay = kFirstVirtualDisplay;
  // One headless primary display. Hotplug of more displays (e.g. one per
  // Wayland output) would add entries here and call onHotplugEvent.
  const DisplayConfig config = DisplayConfig::fromProperties();
  mDisplays.emplace(
    kPrimaryDisplay,
    std::make_unique<Display>(
      kPrimaryDisplay, config,
      createPresenter(
        kPrimaryDisplay, uint32_t(config.width), uint32_t(config.height)
      )
    )
  );
}

ComposerClient::~ComposerClient() {
  LOGI("composer client destroyed");
  {
    std::lock_guard lock(mMutex);
    mDisplays.clear();
  }
  if (mOnDestroyed) mOnDestroyed();
}

ndk::SpAIBinder ComposerClient::createBinder() {
  auto binder = BnComposerClient::createBinder();
  AIBinder_setInheritRt(binder.get(), true);
  return binder;
}

Display* ComposerClient::display(int64_t id) {
  auto it = mDisplays.find(id);
  return it == mDisplays.end() ? nullptr : it->second.get();
}

#define DISPLAY_OR_RETURN()        \
  std::lock_guard lock(mMutex);    \
  Display* d = display(displayId); \
  if (!d) return toStatus(Error::BadDisplay);

// ---- layers ----

ndk::ScopedAStatus ComposerClient::createLayer(
  int64_t displayId, int32_t bufferSlotCount, int64_t* out
) {
  DISPLAY_OR_RETURN();
  *out = d->createLayer();
  if (Layer* l = d->layer(*out))
    l->slots.resize(size_t(std::max(bufferSlotCount, 0)));
  return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::destroyLayer(
  int64_t displayId, int64_t layer
) {
  DISPLAY_OR_RETURN();
  return toStatus(d->destroyLayer(layer));
}

// HWC-backed virtual displays (SurfaceFlinger uses them when
// debug.sf.enable_hwc_vds=1, set by our .rc). Every layer stays CLIENT, so
// SurfaceFlinger renders each frame straight into the sink's buffer and hands
// it to us as both client target and output buffer; the presenter shows it in
// the Wayland window that the window manager asked for (docs/windowing.md).
ndk::ScopedAStatus ComposerClient::createVirtualDisplay(
  int32_t width, int32_t height, common::PixelFormat format,
  int32_t outputSlotCount, VirtualDisplay* out
) {
  if (width < 1 || height < 1 || width > 16384 || height > 16384)
    return toStatus(Error::BadParameter);
  std::lock_guard lock(mMutex);
  size_t virtuals = 0;
  for (auto& [id, d] : mDisplays) virtuals += d->isVirtual();
  if (virtuals >= size_t(kMaxVirtualDisplays))
    return toStatus(Error::NoResources);
  const int64_t id = mNextVirtualDisplay++;
  DisplayConfig config;
  config.id = 0;
  config.width = width;
  config.height = height;
  auto d = std::make_unique<Display>(
    id, config, createVirtualPresenter(id, uint32_t(width), uint32_t(height)),
    /*isVirtual=*/true
  );
  d->setClientTargetSlotCount(std::max(outputSlotCount, 1));
  if (mCallback) d->vsync().setCallback(mCallback);
  mDisplays.emplace(id, std::move(d));
  out->display = id;
  // The sink decides the format; "implementation defined" sinks (an
  // ImageReader with PRIVATE format) get RGBA_8888 from our gralloc anyway.
  out->format = format == common::PixelFormat::IMPLEMENTATION_DEFINED
                  ? common::PixelFormat::RGBA_8888
                  : format;
  LOGI(
    "virtual display %lld created: %dx%d format %d, %d output slots",
    (long long)id, width, height, int(format), outputSlotCount
  );
  return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::destroyVirtualDisplay(int64_t displayId) {
  std::unique_ptr<Display> gone;
  {
    std::lock_guard lock(mMutex);
    auto it = mDisplays.find(displayId);
    if (it == mDisplays.end() || !it->second->isVirtual())
      return toStatus(Error::BadDisplay);
    gone = std::move(it->second);
    mDisplays.erase(it);
  }
  LOGI("virtual display %lld destroyed", (long long)displayId);
  return ndk::ScopedAStatus::ok();  // `gone` (and its window) go away here
}

ndk::ScopedAStatus ComposerClient::getMaxVirtualDisplayCount(int32_t* out) {
  *out = kMaxVirtualDisplays;
  return ndk::ScopedAStatus::ok();
}

// ---- commands ----

ndk::ScopedAStatus ComposerClient::executeCommands(
  const std::vector<DisplayCommand>& commands,
  std::vector<CommandResultPayload>* out
) {
  std::lock_guard lock(mMutex);
  Results results(out);
  for (size_t i = 0; i < commands.size(); i++) {
    results.setIndex(int32_t(i));
    executeDisplayCommand(results, commands[i]);
  }
  return ndk::ScopedAStatus::ok();
}

void ComposerClient::executeDisplayCommand(
  Results& results, const DisplayCommand& cmd
) {
  Display* d = display(cmd.display);
  if (!d) {
    results.error(Error::BadDisplay);
    return;
  }
  for (const LayerCommand& lc : cmd.layers)
    executeLayerCommand(results, *d, lc);

  if (cmd.colorTransformMatrix) d->setColorTransform(*cmd.colorTransformMatrix);
  if (cmd.brightness) {
    // No BRIGHTNESS capability is advertised; accept and ignore.
  }
  if (cmd.activeConfig && cmd.activeConfig->configId != d->config().id) {
    results.error(Error::BadConfig);
  }
  if (cmd.clientTarget) {
    results.error(
      d->setClientTarget(cmd.clientTarget->buffer, cmd.clientTarget->dataspace)
    );
  }
  if (cmd.virtualDisplayOutputBuffer) {
    results.error(
      d->isVirtual() ? d->setOutputBuffer(*cmd.virtualDisplayOutputBuffer)
                     : Error::BadDisplay
    );
  }

  auto validate = [&](bool presentOrValidate) {
    std::optional<ChangedCompositionTypes> changes;
    Error e = d->validate(&changes);
    if (e != Error::None) {
      results.error(e);
      return;
    }
    if (changes) results.add(std::move(*changes));
    if (presentOrValidate) {
      PresentOrValidate pov;
      pov.display = d->id();
      pov.result = PresentOrValidate::Result::Validated;
      results.add(std::move(pov));
    }
  };
  if (cmd.validateDisplay) validate(false);
  if (cmd.acceptDisplayChanges) results.error(d->acceptChanges());
  if (cmd.presentOrValidateDisplay) validate(true);
  if (cmd.presentDisplay) {
    PresentResult pr;
    Error e = d->present(&pr);
    if (e != Error::None) {
      results.error(e);
      return;
    }
    if (pr.presentFence.ok()) {
      PresentFence pf;
      pf.display = d->id();
      pf.fence = ndk::ScopedFileDescriptor(pr.presentFence.release());
      results.add(std::move(pf));
    }
    ReleaseFences rf;
    rf.display = d->id();
    for (auto& [layerId, fence] : pr.releaseFences) {
      ReleaseFences::Layer l;
      l.layer = layerId;
      l.fence = ndk::ScopedFileDescriptor(fence.release());
      rf.layers.push_back(std::move(l));
    }
    results.add(std::move(rf));
  }
}

void ComposerClient::executeLayerCommand(
  Results& results, Display& d, const LayerCommand& c
) {
  Layer* l = d.layer(c.layer);
  if (!l) {
    results.error(Error::BadLayer);
    return;
  }
  if (c.bufferSlotsToClear) {
    for (int32_t s : *c.bufferSlotsToClear) l->slots.clear(s);
  }
  if (c.buffer) {
    BufferPtr b;
    Error e = l->slots.update(*c.buffer, &b);
    if (e == Error::None) {
      l->buffer = std::move(b);
      l->acquireFence.reset(dupFd(c.buffer->fence.get()));
    }
    results.error(e);
  }
  if (c.composition) {
    l->requested = c.composition->composition;
    d.invalidate();
  }
  if (c.z) {
    l->z = c.z->z;
    d.invalidate();
  }
  if (c.displayFrame) l->displayFrame = *c.displayFrame;
  if (c.sourceCrop) l->sourceCrop = *c.sourceCrop;
  if (c.transform) l->transform = c.transform->transform;
  if (c.blendMode) l->blendMode = c.blendMode->blendMode;
  if (c.planeAlpha) l->planeAlpha = c.planeAlpha->alpha;
  if (c.dataspace) l->dataspace = c.dataspace->dataspace;
  if (c.color) l->color = *c.color;
  if (c.sidebandStream) results.error(Error::Unsupported);
  // cursorPosition, damage, visibleRegion, colorTransform, brightness,
  // perFrameMetadata(Blob), blockingRegion, luts, pictureProfileId: only
  // relevant to DEVICE composition, which the headless backend never does.
}

// ---- display queries ----

ndk::ScopedAStatus ComposerClient::getActiveConfig(
  int64_t displayId, int32_t* out
) {
  DISPLAY_OR_RETURN();
  *out = d->config().id;
  return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::getDisplayConfigs(
  int64_t displayId, std::vector<int32_t>* out
) {
  DISPLAY_OR_RETURN();
  *out = {d->config().id};
  return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::getDisplayConfigurations(
  int64_t displayId, int32_t, std::vector<DisplayConfiguration>* out
) {
  DISPLAY_OR_RETURN();
  const DisplayConfig& c = d->config();
  DisplayConfiguration dc;
  dc.configId = c.id;
  dc.width = c.width;
  dc.height = c.height;
  dc.dpi = DisplayConfiguration::Dpi{float(c.dpi), float(c.dpi)};
  dc.configGroup = 0;
  dc.vsyncPeriod = c.vsyncPeriodNs;
  dc.hdrOutputType = OutputType::SYSTEM;
  *out = {dc};
  return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::getDisplayAttribute(
  int64_t displayId, int32_t config, DisplayAttribute attribute, int32_t* out
) {
  DISPLAY_OR_RETURN();
  const DisplayConfig& c = d->config();
  if (config != c.id) return toStatus(Error::BadConfig);
  switch (attribute) {
    case DisplayAttribute::WIDTH:
      *out = c.width;
      break;
    case DisplayAttribute::HEIGHT:
      *out = c.height;
      break;
    case DisplayAttribute::VSYNC_PERIOD:
      *out = c.vsyncPeriodNs;
      break;
    case DisplayAttribute::DPI_X:
    case DisplayAttribute::DPI_Y:
      *out = c.dpi * 1000;
      break;
    case DisplayAttribute::CONFIG_GROUP:
      *out = 0;
      break;
    default:
      return toStatus(Error::BadParameter);
  }
  return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::getDisplayVsyncPeriod(
  int64_t displayId, int32_t* out
) {
  DISPLAY_OR_RETURN();
  *out = d->config().vsyncPeriodNs;
  return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::getDisplayKnownVsyncSample(
  int64_t displayId, VsyncSample* out
) {
  DISPLAY_OR_RETURN();
  out->timestampNs = d->vsync().lastVsyncNs();
  out->vsyncPeriodNs = d->vsync().period();
  return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::setActiveConfig(
  int64_t displayId, int32_t config
) {
  DISPLAY_OR_RETURN();
  return toStatus(config == d->config().id ? Error::None : Error::BadConfig);
}

ndk::ScopedAStatus ComposerClient::setActiveConfigWithConstraints(
  int64_t displayId, int32_t config,
  const VsyncPeriodChangeConstraints& constraints,
  VsyncPeriodChangeTimeline* out
) {
  DISPLAY_OR_RETURN();
  if (config != d->config().id) return toStatus(Error::BadConfig);
  out->newVsyncAppliedTimeNanos =
    std::max(nowNs(), constraints.desiredTimeNanos);
  out->refreshRequired = false;
  out->refreshTimeNanos = 0;
  return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::setBootDisplayConfig(int64_t, int32_t) {
  return toStatus(Error::Unsupported);
}
ndk::ScopedAStatus ComposerClient::clearBootDisplayConfig(int64_t) {
  return toStatus(Error::Unsupported);
}
ndk::ScopedAStatus ComposerClient::getPreferredBootDisplayConfig(
  int64_t, int32_t*
) {
  return toStatus(Error::Unsupported);
}

ndk::ScopedAStatus ComposerClient::getDisplayCapabilities(
  int64_t displayId, std::vector<DisplayCapability>* out
) {
  DISPLAY_OR_RETURN();
  // We never apply color transforms ourselves (so no
  // SKIP_CLIENT_COLOR_TRANSFORM).
  *out = {DisplayCapability::MULTI_THREADED_PRESENT};
  return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::getDisplayConnectionType(
  int64_t displayId, DisplayConnectionType* out
) {
  DISPLAY_OR_RETURN();
  *out = DisplayConnectionType::INTERNAL;
  return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::getDisplayIdentificationData(
  int64_t displayId, DisplayIdentification*
) {
  DISPLAY_OR_RETURN();
  // No EDID: SurfaceFlinger falls back to a port-based display id.
  return toStatus(Error::Unsupported);
}

ndk::ScopedAStatus ComposerClient::getDisplayName(
  int64_t displayId, std::string* out
) {
  DISPLAY_OR_RETURN();
  *out = "Linux.apk";
  return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::getDisplayPhysicalOrientation(
  int64_t displayId, common::Transform* out
) {
  DISPLAY_OR_RETURN();
  *out = common::Transform::NONE;
  return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::getColorModes(
  int64_t displayId, std::vector<ColorMode>* out
) {
  DISPLAY_OR_RETURN();
  *out = {ColorMode::NATIVE};
  return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::getRenderIntents(
  int64_t displayId, ColorMode mode, std::vector<RenderIntent>* out
) {
  DISPLAY_OR_RETURN();
  if (mode != ColorMode::NATIVE) return toStatus(Error::BadParameter);
  *out = {RenderIntent::COLORIMETRIC};
  return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::setColorMode(
  int64_t displayId, ColorMode mode, RenderIntent intent
) {
  DISPLAY_OR_RETURN();
  if (mode != ColorMode::NATIVE || intent != RenderIntent::COLORIMETRIC) {
    return toStatus(Error::Unsupported);
  }
  return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::getDataspaceSaturationMatrix(
  common::Dataspace dataspace, std::vector<float>* out
) {
  if (dataspace != common::Dataspace::SRGB_LINEAR)
    return toStatus(Error::BadParameter);
  *out = {1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1};
  return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::getHdrCapabilities(
  int64_t displayId, HdrCapabilities* out
) {
  DISPLAY_OR_RETURN();
  *out = HdrCapabilities{};
  return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::getPerFrameMetadataKeys(
  int64_t, std::vector<PerFrameMetadataKey>* out
) {
  out->clear();  // no HDR metadata (SurfaceFlinger logs an error otherwise)
  return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::getReadbackBufferAttributes(
  int64_t, ReadbackBufferAttributes*
) {
  return toStatus(Error::Unsupported);
}

ndk::ScopedAStatus ComposerClient::getReadbackBufferFence(
  int64_t, ndk::ScopedFileDescriptor*
) {
  return toStatus(Error::Unsupported);
}

ndk::ScopedAStatus ComposerClient::setReadbackBuffer(
  int64_t, const NativeHandle&, const ndk::ScopedFileDescriptor&
) {
  return toStatus(Error::Unsupported);
}

ndk::ScopedAStatus ComposerClient::getSupportedContentTypes(
  int64_t displayId, std::vector<ContentType>* out
) {
  DISPLAY_OR_RETURN();
  out->clear();
  return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::setContentType(
  int64_t displayId, ContentType type
) {
  DISPLAY_OR_RETURN();
  return toStatus(type == ContentType::NONE ? Error::None : Error::Unsupported);
}

ndk::ScopedAStatus ComposerClient::getDisplayDecorationSupport(
  int64_t displayId, std::optional<common::DisplayDecorationSupport>* out
) {
  DISPLAY_OR_RETURN();
  out->reset();
  return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::getDisplayedContentSample(
  int64_t, int64_t, int64_t, DisplayContentSample*
) {
  return toStatus(Error::Unsupported);
}

ndk::ScopedAStatus ComposerClient::getDisplayedContentSamplingAttributes(
  int64_t, DisplayContentSamplingAttributes*
) {
  return toStatus(Error::Unsupported);
}

ndk::ScopedAStatus ComposerClient::setDisplayedContentSamplingEnabled(
  int64_t, bool, FormatColorComponent, int64_t
) {
  return toStatus(Error::Unsupported);
}

ndk::ScopedAStatus ComposerClient::setAutoLowLatencyMode(int64_t, bool) {
  return toStatus(Error::Unsupported);
}

ndk::ScopedAStatus ComposerClient::setClientTargetSlotCount(
  int64_t displayId, int32_t count
) {
  DISPLAY_OR_RETURN();
  if (count < 0) return toStatus(Error::BadParameter);
  d->setClientTargetSlotCount(count);
  return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::setPowerMode(
  int64_t displayId, PowerMode mode
) {
  DISPLAY_OR_RETURN();
  return toStatus(d->setPowerMode(mode));
}

ndk::ScopedAStatus ComposerClient::setVsyncEnabled(
  int64_t displayId, bool enabled
) {
  DISPLAY_OR_RETURN();
  d->vsync().setEnabled(enabled);
  return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::setIdleTimerEnabled(int64_t, int32_t) {
  return toStatus(Error::Unsupported);
}

ndk::ScopedAStatus ComposerClient::getOverlaySupport(OverlayProperties* out) {
  *out = OverlayProperties{};
  return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::getHdrConversionCapabilities(
  std::vector<common::HdrConversionCapability>*
) {
  return toStatus(Error::Unsupported);
}

ndk::ScopedAStatus ComposerClient::setHdrConversionStrategy(
  const common::HdrConversionStrategy&, common::Hdr*
) {
  return toStatus(Error::Unsupported);
}

ndk::ScopedAStatus ComposerClient::setRefreshRateChangedCallbackDebugEnabled(
  int64_t, bool
) {
  return toStatus(Error::Unsupported);
}

ndk::ScopedAStatus ComposerClient::notifyExpectedPresent(
  int64_t, const ClockMonotonicTimestamp&, int32_t
) {
  return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::getMaxLayerPictureProfiles(
  int64_t displayId, int32_t* out
) {
  DISPLAY_OR_RETURN();
  *out = 0;
  return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::startHdcpNegotiation(
  int64_t, const HdcpLevels&
) {
  return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::getLuts(
  int64_t, const std::vector<Buffer>&, std::vector<Luts>*
) {
  return toStatus(Error::Unsupported);
}

// ---- callbacks ----

ndk::ScopedAStatus ComposerClient::registerCallback(
  const std::shared_ptr<IComposerCallback>& cb
) {
  std::vector<int64_t> ids;
  bool first;
  {
    std::lock_guard lock(mMutex);
    first = mCallback == nullptr;
    mCallback = cb;
    for (auto& [id, d] : mDisplays) {
      d->vsync().setCallback(cb);
      if (!d->isVirtual()) ids.push_back(id);
    }
  }
  if (first && cb) {
    for (int64_t id : ids) {
      LOGI("hotplug: display %lld connected", (long long)id);
      auto st = cb->onHotplugEvent(id, common::DisplayHotplugEvent::CONNECTED);
      if (!st.isOk())
        LOGW("onHotplugEvent failed: %s", st.getDescription().c_str());
    }
  }
  return ndk::ScopedAStatus::ok();
}

}  // namespace linuxapk::hwc
