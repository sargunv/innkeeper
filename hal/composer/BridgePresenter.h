// SPDX-License-Identifier: AGPL-3.0-or-later
// Windows through the host bridge's display service (docs/bridge.md,
// "Display service"; docs/windowing.md): the composer keeps the HWC side and
// the window registry, and the bridge (linuxapk-bridge, Rust, on the host)
// owns the host compositor connection, decorations (server- or client-side) and
// the seat. Frames go over as dma-buf fds of the composer's own udmabuf pool;
// input comes back as protocol v2 lines, which the composer relays on its
// input socket unchanged.
#pragma once

#include <memory>
#include <string>

#include "Presenter.h"

namespace linuxapk::hwc {

class HeadlessPresenter;

// Serves the input socket, and connects to the bridge at `bridgeSocket` in
// the background: windows wait while it is unreachable or restarting.
void startBridge(
  const std::string& bridgeSocket, const std::string& inputSocket
);
bool bridgeStarted();

// Display 0's size, for the input stream's hello line.
void bridgeSetPrimaryDisplaySize(uint32_t width, uint32_t height);
// A virtual display's window, if the window manager asked for one of this
// size; nullptr otherwise. `secondary` gets every frame too (frame dumps).
std::unique_ptr<Presenter> createBridgeVirtualPresenter(
  int64_t hwcDisplayId, uint32_t width, uint32_t height,
  std::unique_ptr<HeadlessPresenter> secondary
);

}  // namespace linuxapk::hwc
