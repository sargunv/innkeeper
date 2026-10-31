// SPDX-License-Identifier: AGPL-3.0-or-later
// Where composed frames go. Every layer is CLIENT: SurfaceFlinger's
// RenderEngine composites them into the client target, and the Presenter
// shows that. The bridge imports the sink's dma-buf and the system app owns
// its Image until Wayland releases it. Mixed virtual composition is blitted
// into the sink on the GPU before submission.
//
// HeadlessPresenter optionally dumps the client target to PNG.
#pragma once

#include <memory>
#include <string>
#include <utility>

#include "Buffer.h"
#include "Common.h"
#include "Fence.h"

namespace linuxapk::hwc {

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

class FrameDumper;
class GpuCopy;

// No output; optional rate-limited PNG dumps of the client target.
class HeadlessPresenter : public Presenter {
 public:
  explicit HeadlessPresenter(std::unique_ptr<FrameDumper> dumper);
  ~HeadlessPresenter() override;
  const char* name() const override { return "headless"; }
  void present(const FrameInput& frame) override;
  // Dump file prefix (default "display<hwc id>").
  void setDumpPrefix(std::string prefix) { mPrefix = std::move(prefix); }

 private:
  struct Worker;
  std::unique_ptr<FrameDumper> mDumper;
  std::unique_ptr<Worker> mWorker;
  BufferPtr mReadback;
  std::unique_ptr<GpuCopy> mReadbackCopy;
  uint64_t mPresented = 0;
  int64_t mLastLogNs = 0;
  std::string mPrefix;
};

// The presenters' backend, from vendor.linuxapk.hwc.backend, read once at
// startup: "bridge" shows virtual displays in host windows through the
// bridge's display service; "headless" (the default) shows none.
void startPresenterBackend();
struct DisplayConfig;
// Display 0 has no window: apps show in windows of the virtual displays that
// the window manager (the Linux.apk System app) creates. In phone mode it is
// the device's screen, in the window the window manager asks for it.
std::unique_ptr<Presenter> createPrimaryPresenter(const DisplayConfig& config);

// Display 0's mode: 1280x800 at 160 dpi, or in phone mode the device's
// screen (vendor.linuxapk.hwc.primary, WxH, and ro.sf.lcd_density).
DisplayConfig primaryConfig();
// A HWC-backed virtual display (ComposerClient::createVirtualDisplay): a
// window if the backend is "bridge" and the bridge has one for it.
std::unique_ptr<Presenter> createVirtualPresenter(
  int64_t displayId, uint32_t width, uint32_t height
);

}  // namespace linuxapk::hwc
