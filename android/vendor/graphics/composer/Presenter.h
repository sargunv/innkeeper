// SPDX-License-Identifier: AGPL-3.0-or-later
// Where composed frames go. Every layer is CLIENT: SurfaceFlinger's
// RenderEngine composites them into the client target, and the Presenter
// shows that. It holds no buffer after present() returns, so there are no
// present or release fences.
//
// HeadlessPresenter: the client target is optionally dumped to PNG.
// BridgePresenter.h: the client target is copied into a window's buffer pool
// and shown on the host through the bridge.
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
};

class Presenter {
 public:
  virtual ~Presenter() = default;
  virtual const char* name() const = 0;
  virtual void present(const FrameInput& frame) = 0;
};

class FrameDumper;

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
  uint64_t mPresented = 0;
  int64_t mLastLogNs = 0;
  std::string mPrefix;
};

// The presenters' backend, from vendor.linuxapk.hwc.backend, read once at
// startup: "bridge" shows virtual displays in host windows through the
// bridge's display service; "headless" (the default) shows none.
void startPresenterBackend();
// Display 0 never has a window: apps show in windows of the virtual
// displays that the window manager (the Linux.apk System app) creates.
std::unique_ptr<Presenter> createPrimaryPresenter();
// A HWC-backed virtual display (ComposerClient::createVirtualDisplay): a
// window if the backend is "bridge" and the bridge has one for it.
std::unique_ptr<Presenter> createVirtualPresenter(
  int64_t displayId, uint32_t width, uint32_t height
);

}  // namespace linuxapk::hwc
