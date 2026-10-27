// SPDX-License-Identifier: AGPL-3.0-or-later
// Windows through the host bridge's display service (docs/bridge.md,
// "Display service"; docs/windowing.md): the composer keeps the HWC side and
// the window registry, and the bridge (linuxapk-bridge, Rust, on the host)
// owns the Wayland connection, decorations (server- or client-side) and the
// seat. Frames go over as dma-buf fds of the composer's own udmabuf pool;
// input comes back as protocol v2 lines, which the composer relays on its
// input socket unchanged.
#pragma once

#include <memory>
#include <string>

#include "Presenter.h"

namespace linuxapk::hwc {

class HeadlessPresenter;

// Connects to the bridge at `bridgeSocket` and serves the input socket.
// Returns false if the bridge is unreachable or refuses the display service.
bool initBridge(
  const std::string& bridgeSocket, const std::string& inputSocket
);
bool bridgeConnected();

// Same contracts as the Wayland presenters (WaylandPresenter.h).
void bridgeSetPrimaryDisplaySize(uint32_t width, uint32_t height);
std::unique_ptr<Presenter> createBridgePresenter(
  int64_t displayId, uint32_t width, uint32_t height,
  std::unique_ptr<HeadlessPresenter> secondary
);
std::unique_ptr<Presenter> createBridgeVirtualPresenter(
  int64_t hwcDisplayId, uint32_t width, uint32_t height,
  std::unique_ptr<HeadlessPresenter> secondary
);

}  // namespace linuxapk::hwc
