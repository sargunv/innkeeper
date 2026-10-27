// SPDX-License-Identifier: AGPL-3.0-or-later
// One HWC3 display: config, layers, validate/present state machine.
// Not thread safe; ComposerClient serialises access.
#pragma once

#include <map>
#include <memory>
#include <optional>
#include <string>

#include <aidl/android/hardware/graphics/composer3/ChangedCompositionTypes.h>
#include <aidl/android/hardware/graphics/composer3/DisplayConfiguration.h>
#include <aidl/android/hardware/graphics/composer3/PowerMode.h>

#include "Buffer.h"
#include "Common.h"
#include "Presenter.h"
#include "VsyncThread.h"

namespace linuxapk::hwc {

// The defaults are display 0's: it has no window, so its size only matters
// to what SurfaceFlinger and the framework derive from it.
struct DisplayConfig {
  int32_t id = 0;
  int32_t width = 1280, height = 800;
  int32_t vsyncPeriodNs = 16'666'666;
  int32_t dpi = 160;
};

// SurfaceFlinger composites every layer itself (CLIENT), so a layer is only
// the composition it asked for: its buffer and geometry never reach us.
struct Layer {
  c3::Composition requested = c3::Composition::INVALID;
};

class Display {
 public:
  // Virtual displays (createVirtualDisplay) start powered on and get their
  // frames' output buffer from setOutputBuffer.
  Display(
    int64_t id, DisplayConfig config, std::unique_ptr<Presenter> presenter,
    bool isVirtual = false
  );

  int64_t id() const { return mId; }
  bool isVirtual() const { return mVirtual; }
  const DisplayConfig& config() const { return mConfig; }
  VsyncThread& vsync() { return mVsync; }

  int64_t createLayer();
  Error destroyLayer(int64_t layer);
  Layer* layer(int64_t id);

  void setClientTargetSlotCount(int32_t n) {
    mClientTargetSlots.resize(size_t(n));
  }
  Error setClientTarget(const c3::Buffer& buffer);
  Error setOutputBuffer(const c3::Buffer& buffer);
  Error setPowerMode(c3::PowerMode mode);
  void setColorTransform(const std::vector<float>& m);

  // validate: returns the composition changes (if any) SF must accept.
  Error validate(std::optional<c3::ChangedCompositionTypes>* changes);
  Error acceptChanges();
  Error present();
  // Any layer/display state change invalidates a previous validate.
  void invalidate() { mState = State::NeedValidate; }

 private:
  enum class State { NeedValidate, NeedAccept, ReadyToPresent };

  const int64_t mId;
  const bool mVirtual;
  DisplayConfig mConfig;
  std::unique_ptr<Presenter> mPresenter;
  VsyncThread mVsync;
  std::map<int64_t, Layer> mLayers;
  int64_t mNextLayerId = 1;
  BufferSlots mClientTargetSlots;
  ClientTargetState mClientTarget;
  BufferSlots mOutputSlots;
  BufferPtr mOutputBuffer;
  State mState = State::NeedValidate;
  std::vector<int64_t> mPendingChanges;  // layers that become CLIENT
  c3::PowerMode mPower = c3::PowerMode::OFF;
};

}  // namespace linuxapk::hwc
