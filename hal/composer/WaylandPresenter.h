// SPDX-License-Identifier: AGPL-3.0-or-later
// Presents display 0 in a window of the host's Wayland compositor and turns
// the window's wl_seat input into the InputServer event stream.
//
// WaylandConnection is a process singleton: it connects once (at startup,
// while the composer still runs as root, which owns the host socket), binds
// the globals and runs the event thread. Each WaylandPresenter (one per
// composer client, i.e. per SurfaceFlinger instance) owns a toplevel window.
#pragma once

#include <memory>
#include <string>

#include "Presenter.h"

namespace linuxapk::hwc {

class WaylandWindow;

class WaylandConnection {
 public:
  // Connects to `socketPath` (vendor.linuxapk.wayland.socket). Returns false
  // (and logs) if the compositor is unreachable or lacks required globals.
  static bool init(
    const std::string& socketPath, const std::string& inputSocketPath
  );
  static WaylandConnection* get();  // nullptr unless init() succeeded

  std::unique_ptr<WaylandWindow> createWindow(
    int64_t displayId, uint32_t width, uint32_t height
  );

  struct Impl;
  explicit WaylandConnection(std::unique_ptr<Impl> impl);
  ~WaylandConnection();

 private:
  std::unique_ptr<Impl> mImpl;
};

class WaylandPresenter : public Presenter {
 public:
  // `secondary` (e.g. the headless dumper) also sees every frame.
  WaylandPresenter(
    std::unique_ptr<WaylandWindow> window, std::unique_ptr<Presenter> secondary
  );
  ~WaylandPresenter() override;
  const char* name() const override { return "wayland"; }
  Error present(FrameInput& frame, PresentResult* out) override;

 private:
  std::unique_ptr<WaylandWindow> mWindow;
  std::unique_ptr<Presenter> mSecondary;
};

// nullptr if there is no connection or the window can't be created.
std::unique_ptr<Presenter> createWaylandPresenter(
  int64_t displayId, uint32_t width, uint32_t height,
  std::unique_ptr<Presenter> secondary
);

}  // namespace linuxapk::hwc
