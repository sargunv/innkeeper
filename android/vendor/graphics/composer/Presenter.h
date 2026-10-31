// SPDX-License-Identifier: AGPL-3.0-or-later
// Where composed frames go. Content layers are CLIENT: SurfaceFlinger's
// RenderEngine composites them into the client target, and the Presenter
// shows that; a CURSOR layer is passed separately. The bridge imports the
// sink's dma-buf and the system app owns its Image until Wayland releases it.
// Mixed virtual composition is blitted into the sink on the GPU before
// submission.
#pragma once

#include <memory>

#include "Buffer.h"
#include "Common.h"
#include "Fence.h"

namespace innkeeper::hwc {

struct ClientTargetState {
  BufferPtr buffer;
  UniqueFd acquireFence;
};

struct CursorState {
  BufferPtr buffer;
  UniqueFd acquireFence;
  int32_t x = 0, y = 0;
  float alpha = 1;
};

struct FrameInput {
  int64_t displayId;
  ClientTargetState* clientTarget;
  BufferPtr outputBuffer;  // virtual displays: the sink's buffer
  CursorState* cursor = nullptr;
  bool clientTargetUpdated = true;
};

class Presenter {
 public:
  virtual ~Presenter() = default;
  virtual const char* name() const = 0;
  virtual void present(const FrameInput& frame) = 0;
};

// No output; logs how many frames it was given.
class HeadlessPresenter : public Presenter {
 public:
  const char* name() const override { return "headless"; }
  void present(const FrameInput& frame) override;

 private:
  uint64_t mPresented = 0;
  int64_t mLastLogNs = 0;
};

// The presenters' backend, from vendor.innkeeper.hwc.backend, read once at
// startup: "bridge" shows virtual displays in host windows through the
// bridge's display service; "headless" (the default) shows none.
void startPresenterBackend();
struct DisplayConfig;
// Display 0 has no window: apps show in windows of the virtual displays that
// the window manager (the Innkeeper System app) creates. In phone mode it is
// the device's screen, in the window the window manager asks for it.
std::unique_ptr<Presenter> createPrimaryPresenter(const DisplayConfig& config);

// Display 0's mode: 1280x800 at 160 dpi, or in phone mode the device's
// screen (vendor.innkeeper.hwc.primary, WxH, and ro.sf.lcd_density).
DisplayConfig primaryConfig();
// A HWC-backed virtual display (ComposerClient::createVirtualDisplay): a
// window if the backend is "bridge" and the bridge has one for it.
std::unique_ptr<Presenter> createVirtualPresenter(
  int64_t displayId, uint32_t width, uint32_t height
);

}  // namespace innkeeper::hwc
