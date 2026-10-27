// SPDX-License-Identifier: AGPL-3.0-or-later
// Presents displays as windows of the host's Wayland compositor and turns the
// windows' wl_seat input into the InputServer event stream.
//
// WaylandConnection is a process singleton: it connects once (at startup,
// while the composer still runs as root, which owns the host socket), binds
// the globals and runs the event thread. It also keeps the window registry:
//   - display 0 gets a window only in single-window mode;
//   - a HWC virtual display gets a window when the window manager (the
//     Linux.apk System app) asked for one over the input socket
//     ("window_new", docs/hal-display.md) before SurfaceFlinger created it.
// Windows are keyed by the Android display id they show, which is also what
// input events carry.
#pragma once

#include <memory>
#include <string>

#include "Presenter.h"

namespace linuxapk::hwc {

class WaylandWindow;
class HeadlessPresenter;

class WaylandConnection {
 public:
  // Connects to `socketPath` (vendor.linuxapk.wayland.socket). Returns false
  // (and logs) if the compositor is unreachable or lacks required globals.
  static bool init(
    const std::string& socketPath, const std::string& inputSocketPath
  );
  static WaylandConnection* get();  // nullptr unless init() succeeded

  struct Impl;
  explicit WaylandConnection(std::unique_ptr<Impl> impl);
  ~WaylandConnection();
  Impl* impl() { return mImpl.get(); }

 private:
  std::unique_ptr<Impl> mImpl;
};

// Display 0's size, for the input stream's hello line.
void setPrimaryDisplaySize(uint32_t width, uint32_t height);
// nullptr if there is no connection or the window can't be created.
// Display 0's window (single-window mode).
std::unique_ptr<Presenter> createWaylandPresenter(
  int64_t displayId, uint32_t width, uint32_t height,
  std::unique_ptr<HeadlessPresenter> secondary
);
// A virtual display's window, if the window manager asked for one of this
// size; nullptr otherwise (the display stays headless).
std::unique_ptr<Presenter> createWaylandVirtualPresenter(
  int64_t hwcDisplayId, uint32_t width, uint32_t height,
  std::unique_ptr<HeadlessPresenter> secondary
);

}  // namespace linuxapk::hwc
