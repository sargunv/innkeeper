// SPDX-License-Identifier: AGPL-3.0-or-later
#pragma once

#include <aidl/android/hardware/graphics/composer3/BnComposerClient.h>

#include <functional>
#include <map>
#include <memory>
#include <mutex>

#include "Common.h"
#include "Display.h"

namespace linuxapk::hwc {

using namespace ::aidl::android::hardware::graphics::composer3;
using ::aidl::android::hardware::common::NativeHandle;
using ::aidl::android::hardware::drm::HdcpLevels;

class ComposerClient : public BnComposerClient {
  public:
    ComposerClient();
    ~ComposerClient() override;
    void setOnDestroyed(std::function<void()> f) { mOnDestroyed = std::move(f); }

    ndk::ScopedAStatus createLayer(int64_t display, int32_t bufferSlotCount,
                                   int64_t* out) override;
    ndk::ScopedAStatus createVirtualDisplay(int32_t, int32_t, common::PixelFormat, int32_t,
                                            VirtualDisplay*) override;
    ndk::ScopedAStatus destroyLayer(int64_t display, int64_t layer) override;
    ndk::ScopedAStatus destroyVirtualDisplay(int64_t display) override;
    ndk::ScopedAStatus executeCommands(const std::vector<DisplayCommand>& commands,
                                       std::vector<CommandResultPayload>* out) override;
    ndk::ScopedAStatus getActiveConfig(int64_t display, int32_t* out) override;
    ndk::ScopedAStatus getColorModes(int64_t display, std::vector<ColorMode>* out) override;
    ndk::ScopedAStatus getDataspaceSaturationMatrix(common::Dataspace,
                                                    std::vector<float>* out) override;
    ndk::ScopedAStatus getDisplayAttribute(int64_t display, int32_t config,
                                           DisplayAttribute attribute, int32_t* out) override;
    ndk::ScopedAStatus getDisplayCapabilities(int64_t display,
                                              std::vector<DisplayCapability>* out) override;
    ndk::ScopedAStatus getDisplayConfigs(int64_t display, std::vector<int32_t>* out) override;
    ndk::ScopedAStatus getDisplayConnectionType(int64_t display,
                                                DisplayConnectionType* out) override;
    ndk::ScopedAStatus getDisplayIdentificationData(int64_t display,
                                                    DisplayIdentification* out) override;
    ndk::ScopedAStatus getDisplayName(int64_t display, std::string* out) override;
    ndk::ScopedAStatus getDisplayVsyncPeriod(int64_t display, int32_t* out) override;
    ndk::ScopedAStatus getDisplayedContentSample(int64_t, int64_t, int64_t,
                                                 DisplayContentSample*) override;
    ndk::ScopedAStatus getDisplayedContentSamplingAttributes(
            int64_t, DisplayContentSamplingAttributes*) override;
    ndk::ScopedAStatus getDisplayPhysicalOrientation(int64_t display,
                                                     common::Transform* out) override;
    ndk::ScopedAStatus getHdrCapabilities(int64_t display, HdrCapabilities* out) override;
    ndk::ScopedAStatus getMaxVirtualDisplayCount(int32_t* out) override;
    ndk::ScopedAStatus getPerFrameMetadataKeys(int64_t,
                                               std::vector<PerFrameMetadataKey>*) override;
    ndk::ScopedAStatus getReadbackBufferAttributes(int64_t, ReadbackBufferAttributes*) override;
    ndk::ScopedAStatus getReadbackBufferFence(int64_t, ndk::ScopedFileDescriptor*) override;
    ndk::ScopedAStatus getRenderIntents(int64_t display, ColorMode mode,
                                        std::vector<RenderIntent>* out) override;
    ndk::ScopedAStatus getSupportedContentTypes(int64_t display,
                                                std::vector<ContentType>* out) override;
    ndk::ScopedAStatus getDisplayDecorationSupport(
            int64_t display, std::optional<common::DisplayDecorationSupport>* out) override;
    ndk::ScopedAStatus registerCallback(const std::shared_ptr<IComposerCallback>& cb) override;
    ndk::ScopedAStatus setActiveConfig(int64_t display, int32_t config) override;
    ndk::ScopedAStatus setActiveConfigWithConstraints(int64_t display, int32_t config,
                                                      const VsyncPeriodChangeConstraints&,
                                                      VsyncPeriodChangeTimeline* out) override;
    ndk::ScopedAStatus setBootDisplayConfig(int64_t, int32_t) override;
    ndk::ScopedAStatus clearBootDisplayConfig(int64_t) override;
    ndk::ScopedAStatus getPreferredBootDisplayConfig(int64_t, int32_t*) override;
    ndk::ScopedAStatus setAutoLowLatencyMode(int64_t, bool) override;
    ndk::ScopedAStatus setClientTargetSlotCount(int64_t display, int32_t count) override;
    ndk::ScopedAStatus setColorMode(int64_t display, ColorMode mode,
                                    RenderIntent intent) override;
    ndk::ScopedAStatus setContentType(int64_t display, ContentType type) override;
    ndk::ScopedAStatus setDisplayedContentSamplingEnabled(int64_t, bool, FormatColorComponent,
                                                          int64_t) override;
    ndk::ScopedAStatus setPowerMode(int64_t display, PowerMode mode) override;
    ndk::ScopedAStatus setReadbackBuffer(int64_t, const NativeHandle&,
                                         const ndk::ScopedFileDescriptor&) override;
    ndk::ScopedAStatus setVsyncEnabled(int64_t display, bool enabled) override;
    ndk::ScopedAStatus setIdleTimerEnabled(int64_t display, int32_t timeoutMs) override;
    ndk::ScopedAStatus getOverlaySupport(OverlayProperties* out) override;
    ndk::ScopedAStatus getHdrConversionCapabilities(
            std::vector<common::HdrConversionCapability>* out) override;
    ndk::ScopedAStatus setHdrConversionStrategy(const common::HdrConversionStrategy&,
                                                common::Hdr* out) override;
    ndk::ScopedAStatus setRefreshRateChangedCallbackDebugEnabled(int64_t, bool) override;
    ndk::ScopedAStatus getDisplayConfigurations(int64_t display, int32_t maxFrameIntervalNs,
                                                std::vector<DisplayConfiguration>* out) override;
    ndk::ScopedAStatus notifyExpectedPresent(int64_t, const ClockMonotonicTimestamp&,
                                             int32_t) override;
    ndk::ScopedAStatus getMaxLayerPictureProfiles(int64_t display, int32_t* out) override;
    ndk::ScopedAStatus startHdcpNegotiation(int64_t, const HdcpLevels&) override;
    ndk::ScopedAStatus getLuts(int64_t, const std::vector<Buffer>&, std::vector<Luts>*) override;
    ndk::ScopedAStatus getDisplayKnownVsyncSample(int64_t display, VsyncSample* out) override;

  protected:
    ndk::SpAIBinder createBinder() override;

  private:
    class Results;
    Display* display(int64_t id);  // requires mMutex
    void executeDisplayCommand(Results& results, const DisplayCommand& cmd);
    void executeLayerCommand(Results& results, Display& d, const LayerCommand& cmd);

    std::mutex mMutex;
    std::map<int64_t, std::unique_ptr<Display>> mDisplays;
    std::shared_ptr<IComposerCallback> mCallback;
    std::function<void()> mOnDestroyed;
};

}  // namespace linuxapk::hwc
