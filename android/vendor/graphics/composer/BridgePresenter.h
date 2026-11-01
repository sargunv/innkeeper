// SPDX-License-Identifier: AGPL-3.0-or-later
// HWC virtual displays shown in host windows through the display service of
// innkeeper-bridge, the host bridge (host/bridge/src/display/proto.rs). The
// composer adds each virtual display to the bridge, and the bridge shows it in
// the window that the window manager (the Innkeeper System app) asked for at
// that size, if any. Frames travel as the ImageReader sink's dma-buf fds and
// are retained until RELEASE.
#pragma once

#include <memory>

#include "Presenter.h"

namespace innkeeper::hwc {

// Connects to the bridge (/dev/socket/innkeeper/bridge.sock) in the
// background, and again whenever the connection drops. Displays wait while
// the bridge is unreachable or restarting.
void startBridge();
bool bridgeStarted();

// A virtual display, shown if the bridge has a window for it.
std::unique_ptr<Presenter> createBridgeVirtualPresenter(
  int64_t hwcDisplayId, uint32_t width, uint32_t height
);

// Phone mode's display 0 (vendor.innkeeper.mode=phone), shown in the window
// the window manager asks for it.
std::unique_ptr<Presenter> createBridgePrimaryPresenter(
  uint32_t width, uint32_t height
);

}  // namespace innkeeper::hwc
