// SPDX-License-Identifier: AGPL-3.0-or-later
// Where composed frames go. The composer core (Display) owns HWC3 state and
// calls a Presenter for the two backend-specific decisions:
//   1. validate: which layers the backend shows itself (DEVICE). The rest are
//      CLIENT and SurfaceFlinger's RenderEngine composites them into the
//      client target.
//   2. present: show the client target and/or DEVICE layers, returning a
//      present fence and per-layer release fences (-1 = already free).
//
// HeadlessPresenter (M0): everything is CLIENT; the client target is
// optionally dumped to PNG/PPM. A WaylandPresenter (M1) implements the same
// interface: it attaches the client target (or, per layer, each DEVICE
// layer's buffer) to a wl_surface via zwp_linux_dmabuf_v1 (the buffers are
// linear udmabufs, see gralloc_handle.h) or copies into wl_shm.
#pragma once

#include <aidl/android/hardware/graphics/common/Dataspace.h>
#include <aidl/android/hardware/graphics/common/FRect.h>
#include <aidl/android/hardware/graphics/common/Rect.h>
#include <aidl/android/hardware/graphics/common/Transform.h>
#include <aidl/android/hardware/graphics/common/BlendMode.h>
#include <aidl/android/hardware/graphics/composer3/Color.h>
#include <aidl/android/hardware/graphics/composer3/Composition.h>

#include <memory>
#include <string>
#include <utility>
#include <vector>

#include "Buffer.h"
#include "Common.h"
#include "Fence.h"

namespace linuxapk::hwc {

struct LayerState {
    int64_t id = 0;
    c3::Composition requested = c3::Composition::INVALID;  // what SF asked for
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
    ClientTargetState* clientTarget;          // may have no buffer if every layer is DEVICE
    std::vector<LayerState*> layers;          // sorted by z
    bool hasClientLayers;
};

struct PresentResult {
    UniqueFd presentFence;                              // -1: no fence
    std::vector<std::pair<int64_t, UniqueFd>> releaseFences;  // only layers with a fence
};

class Presenter {
  public:
    virtual ~Presenter() = default;
    virtual const char* name() const = 0;
    // Called in validate for layers SF asked to be DEVICE (or cursor, color, ...).
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

  private:
    struct Worker;
    std::unique_ptr<FrameDumper> mDumper;
    std::unique_ptr<Worker> mWorker;
    uint64_t mPresented = 0;
    int64_t mLastLogNs = 0;
};

std::unique_ptr<Presenter> createPresenter(int64_t displayId);

}  // namespace linuxapk::hwc
