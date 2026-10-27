// SPDX-License-Identifier: AGPL-3.0-or-later
// One HWC3 display: config, layers, validate/present state machine.
// Not thread safe; ComposerClient serialises access.
#pragma once

#include <aidl/android/hardware/graphics/composer3/ChangedCompositionTypes.h>
#include <aidl/android/hardware/graphics/composer3/DisplayConfiguration.h>
#include <aidl/android/hardware/graphics/composer3/PowerMode.h>

#include <map>
#include <memory>
#include <optional>
#include <string>

#include "Buffer.h"
#include "Common.h"
#include "Presenter.h"
#include "VsyncThread.h"

namespace linuxapk::hwc {

struct DisplayConfig {
    int32_t id = 0;
    int32_t width = 1280, height = 800;
    int32_t vsyncPeriodNs = 16'666'666;
    int32_t dpi = 160;
    // vendor.linuxapk.display.{width,height,refresh_hz,dpi}
    static DisplayConfig fromProperties();
};

struct Layer : LayerState {
    BufferSlots slots;
};

class Display {
  public:
    Display(int64_t id, DisplayConfig config, std::unique_ptr<Presenter> presenter);

    int64_t id() const { return mId; }
    const DisplayConfig& config() const { return mConfig; }
    VsyncThread& vsync() { return mVsync; }

    int64_t createLayer();
    Error destroyLayer(int64_t layer);
    Layer* layer(int64_t id);

    void setClientTargetSlotCount(int32_t n) { mClientTargetSlots.resize(size_t(n)); }
    Error setClientTarget(const c3::Buffer& buffer, common::Dataspace dataspace);
    Error setPowerMode(c3::PowerMode mode);
    void setColorTransform(const std::vector<float>& m);

    // validate: returns the composition changes (if any) SF must accept.
    Error validate(std::optional<c3::ChangedCompositionTypes>* changes);
    Error acceptChanges();
    Error present(PresentResult* out);
    // Any layer/display state change invalidates a previous validate.
    void invalidate() { mState = State::NeedValidate; }

  private:
    enum class State { NeedValidate, NeedAccept, ReadyToPresent };

    const int64_t mId;
    DisplayConfig mConfig;
    std::unique_ptr<Presenter> mPresenter;
    VsyncThread mVsync;
    std::map<int64_t, std::unique_ptr<Layer>> mLayers;
    int64_t mNextLayerId = 1;
    BufferSlots mClientTargetSlots;
    ClientTargetState mClientTarget;
    State mState = State::NeedValidate;
    std::vector<std::pair<int64_t, c3::Composition>> mPendingChanges;
    c3::PowerMode mPower = c3::PowerMode::OFF;
    uint64_t mFrameNumber = 0;
};

}  // namespace linuxapk::hwc
