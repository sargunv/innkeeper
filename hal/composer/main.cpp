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

#include "Composer.h"
#include "Properties.h"
#include "WaylandPresenter.h"
#include "linuxapk/log.h"

// Started as root (see the .rc) so it can reach the host's Wayland socket,
// which only the session's root (the host user) may connect to. Connect
// first, then become system:graphics.
static void connectAndDropPrivileges() {
  using namespace linuxapk::hwc;
  if (getProperty("vendor.linuxapk.hwc.backend", "headless") == "wayland") {
    WaylandConnection::init(
      getProperty("vendor.linuxapk.wayland.socket", "/dev/socket/wayland-host"),
      getProperty("vendor.linuxapk.input.socket", "/dev/socket/linuxapk-input")
    );
  }
  if (getuid() != 0) return;
  const gid_t groups[] = {1003};  // graphics
  if (
    setgroups(1, groups) != 0 || setresgid(1000, 1000, 1000) != 0 ||
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
