// SPDX-License-Identifier: AGPL-3.0-or-later
// Where composed frames go. The composer core (Display) owns HWC3 state and
// calls a Presenter for the two backend-specific decisions:
//   1. validate: which layers the backend shows itself (DEVICE). The rest are
//      CLIENT and SurfaceFlinger's RenderEngine composites them into the
//      client target.
//   2. present: show the client target and/or DEVICE layers, returning a
//      present fence and per-layer release fences (-1 = already free).
//
// HeadlessPresenter: the client target is optionally dumped to PNG/PPM.
// BridgePresenter.h: the client target is copied into a window's buffer pool
// and shown on the host through the bridge.
#pragma once

#include <memory>
#include <string>
#include <utility>
#include <vector>

#include <aidl/android/hardware/graphics/common/BlendMode.h>
#include <aidl/android/hardware/graphics/common/Dataspace.h>
#include <aidl/android/hardware/graphics/common/FRect.h>
#include <aidl/android/hardware/graphics/common/Rect.h>
#include <aidl/android/hardware/graphics/common/Transform.h>
#include <aidl/android/hardware/graphics/composer3/Color.h>
#include <aidl/android/hardware/graphics/composer3/Composition.h>

#include "Buffer.h"
#include "Common.h"
#include "Fence.h"

namespace linuxapk::hwc {

struct LayerState {
  int64_t id = 0;
  c3::Composition requested = c3::Composition::INVALID;   // what SF asked for
  c3::Composition composition = c3::Composition::CLIENT;  // what we settled on
  int32_t z = 0;
  BufferPtr buffer;
  UniqueFd acquireFence;
  common::Rect displayFrame{};
  common::FRect sourceCrop{};
  common::Transform transform = common::Transform::NONE;
  common::BlendMode blendMode = common::BlendMode::NONE;
  float planeAlpha = 1.f;
  common::Dataspace dataspace = common::Dataspace::UNKNOWN;
  c3::Color color{};
};

struct ClientTargetState {
  BufferPtr buffer;
  UniqueFd acquireFence;
  common::Dataspace dataspace = common::Dataspace::UNKNOWN;
};

struct FrameInput {
  int64_t displayId;
  uint32_t width, height;
  uint64_t frameNumber;
  ClientTargetState*
    clientTarget;          // may have no buffer if every layer is DEVICE
  BufferPtr outputBuffer;  // virtual displays: the sink's buffer
  std::vector<LayerState*> layers;  // sorted by z
  bool hasClientLayers;
};

struct PresentResult {
  UniqueFd presentFence;  // -1: no fence
  std::vector<std::pair<int64_t, UniqueFd>>
    releaseFences;  // only layers with a fence
};

class Presenter {
 public:
  virtual ~Presenter() = default;
  virtual const char* name() const = 0;
  // Called in validate for layers SF asked to be DEVICE (or cursor, color,
  // ...).
  virtual bool acceptDeviceLayer(const LayerState&) { return false; }
  virtual Error present(FrameInput& frame, PresentResult* out) = 0;
  virtual void setPowerOn(bool) {}
};

class FrameDumper;

// No output; optional rate-limited PNG/PPM dumps of the client target.
class HeadlessPresenter : public Presenter {
 public:
  explicit HeadlessPresenter(std::unique_ptr<FrameDumper> dumper);
  ~HeadlessPresenter() override;
  const char* name() const override { return "headless"; }
  Error present(FrameInput& frame, PresentResult* out) override;
  // Dump file prefix (default "display<hwc id>").
  void setDumpPrefix(std::string prefix) { mPrefix = std::move(prefix); }

 private:
  struct Worker;
  std::unique_ptr<FrameDumper> mDumper;
  std::unique_ptr<Worker> mWorker;
  uint64_t mPresented = 0;
  int64_t mLastLogNs = 0;
  std::string mPrefix;
};

// With the bridge started (startBridge), display 0 gets a window only in
// single-window mode (vendor.linuxapk.wm.mode=single); in per-app mode (the
// default) only virtual displays that the window manager asked for get
// windows.
std::unique_ptr<Presenter> createPresenter(
  int64_t displayId, uint32_t width, uint32_t height
);
// A HWC-backed virtual display (ComposerClient::createVirtualDisplay).
std::unique_ptr<Presenter> createVirtualPresenter(
  int64_t displayId, uint32_t width, uint32_t height
);
bool singleWindowMode();

}  // namespace linuxapk::hwc
