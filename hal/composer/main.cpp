// SPDX-License-Identifier: AGPL-3.0-or-later
// android.hardware.graphics.composer3.IComposer/default (AIDL V5).
#define LOG_TAG "linuxapk-hwc"

#include <string>

#include <android/binder_manager.h>
#include <android/binder_process.h>
#include <errno.h>
#include <grp.h>
#include <sched.h>
#include <string.h>
#include <unistd.h>

#include "BridgePresenter.h"
#include "Composer.h"
#include "Properties.h"
#include "WaylandPresenter.h"
#include "linuxapk/log.h"

// Backends with windows (vendor.linuxapk.hwc.backend):
//   bridge:  the host bridge's display service (docs/bridge.md) owns the
//            Wayland connection; falls back to wayland if it is unreachable.
//   wayland: the composer is the Wayland client itself (needs root to reach
//            the host socket, which only the session's root may connect to).
// Connect first, then become system:graphics (plus audio: the bridge socket
// directory is root:audio 0750, and a reconnect happens after the drop).
static void connectAndDropPrivileges() {
  using namespace linuxapk::hwc;
  const std::string backend =
    getProperty("vendor.linuxapk.hwc.backend", "headless");
  const std::string input =
    getProperty("vendor.linuxapk.input.socket", "/dev/socket/linuxapk-input");
  bool connected = false;
  if (backend == "bridge") {
    connected = initBridge(
      getProperty(
        "vendor.linuxapk.bridge.socket", "/dev/socket/linuxapk/bridge.sock"
      ),
      input
    );
    if (!connected)
      LOGW("bridge unavailable; trying the host Wayland socket directly");
  }
  if (!connected && (backend == "bridge" || backend == "wayland")) {
    WaylandConnection::init(
      getProperty("vendor.linuxapk.wayland.socket", "/dev/socket/wayland-host"),
      input
    );
  }
  if (getuid() != 0) return;
  const gid_t groups[] = {1003, 1005};  // graphics, audio
  if (
    setgroups(2, groups) != 0 || setresgid(1000, 1000, 1000) != 0 ||
    setresuid(1000, 1000, 1000) != 0
  ) {
    LOGW("could not drop to system:graphics: %s", strerror(errno));
  }
}

int main() {
  connectAndDropPrivileges();
  // Same priority as SurfaceFlinger's main thread (best effort in a userns).
  struct sched_param param = {.sched_priority = 2};
  sched_setscheduler(0, SCHED_FIFO | SCHED_RESET_ON_FORK, &param);

  ABinderProcess_setThreadPoolMaxThreadCount(5);
  ABinderProcess_startThreadPool();
  auto composer = ndk::SharedRefBase::make<linuxapk::hwc::Composer>();
  const std::string instance =
    std::string(linuxapk::hwc::Composer::descriptor) + "/default";
  binder_status_t st =
    AServiceManager_addService(composer->asBinder().get(), instance.c_str());
  if (st != STATUS_OK) {
    LOGE("failed to register %s: %d", instance.c_str(), st);
    return 1;
  }
  LOGI(
    "registered %s (V%d)", instance.c_str(), linuxapk::hwc::Composer::version
  );
  ABinderProcess_joinThreadPool();
  return 1;
}
