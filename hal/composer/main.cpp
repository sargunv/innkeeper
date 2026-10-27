// SPDX-License-Identifier: AGPL-3.0-or-later
// android.hardware.graphics.composer3.IComposer/default (AIDL V5).
#define LOG_TAG "linuxapk-hwc"

#include <string>

#include <android/binder_manager.h>
#include <android/binder_process.h>
#include <sched.h>

#include "BridgePresenter.h"
#include "Composer.h"
#include "Properties.h"
#include "linuxapk/log.h"

int main() {
  using namespace linuxapk::hwc;
  // vendor.linuxapk.hwc.backend: "bridge" shows windows through the host
  // bridge's display service (docs/bridge.md); "headless" (the default) has
  // none.
  const std::string backend =
    getProperty("vendor.linuxapk.hwc.backend", "headless");
  if (backend == "bridge") {
    startBridge();
  } else if (backend != "headless") {
    LOGW("unknown backend '%s'; using headless", backend.c_str());
  }
  // Same priority as SurfaceFlinger's main thread (best effort in a userns).
  struct sched_param param = {.sched_priority = 2};
  sched_setscheduler(0, SCHED_FIFO | SCHED_RESET_ON_FORK, &param);

  ABinderProcess_setThreadPoolMaxThreadCount(5);
  ABinderProcess_startThreadPool();
  auto composer = ndk::SharedRefBase::make<Composer>();
  const std::string instance = std::string(Composer::descriptor) + "/default";
  binder_status_t st =
    AServiceManager_addService(composer->asBinder().get(), instance.c_str());
  if (st != STATUS_OK) {
    LOGE("failed to register %s: %d", instance.c_str(), st);
    return 1;
  }
  LOGI("registered %s (V%d)", instance.c_str(), Composer::version);
  ABinderProcess_joinThreadPool();
  return 1;
}
