// SPDX-License-Identifier: AGPL-3.0-or-later
// HWC virtual displays shown in host windows through the host bridge's
// display service (linuxapk-bridge, Rust, on the host; host/bridge/src/
// display/proto.rs). The composer only presents: it adds each virtual display
// to the bridge, and the bridge shows it in the window that the window
// manager (the Linux.apk System app) asked for at that size, if any. Frames
// go over as the ImageReader sink's own dma-buf fds, retained until RELEASE.
#pragma once

#include <memory>

#include "Presenter.h"

namespace linuxapk::hwc {

class HeadlessPresenter;

// Connects to the bridge (/dev/socket/linuxapk/bridge.sock) in the
// background, and again whenever the connection drops: displays wait while it
// is unreachable or restarting.
void startBridge();
bool bridgeStarted();

// A virtual display, shown if the bridge has a window for it. `secondary`
// gets every frame too (frame dumps).
std::unique_ptr<Presenter> createBridgeVirtualPresenter(
  int64_t hwcDisplayId, uint32_t width, uint32_t height,
  std::unique_ptr<HeadlessPresenter> secondary
);

}  // namespace linuxapk::hwc
