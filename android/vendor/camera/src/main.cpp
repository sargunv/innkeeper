// SPDX-License-Identifier: AGPL-3.0-or-later
// The camera provider service. It asks the bridge once at start whether the
// desktop has a camera, and lists it if so.
#define LOG_TAG "innkeeper-camera"

#include <android/binder_manager.h>
#include <android/binder_process.h>

#include "HostCamera.h"
#include "Provider.h"
#include "innkeeper/log.h"

int main() {
  ABinderProcess_setThreadPoolMaxThreadCount(4);
  ABinderProcess_startThreadPool();
  bool present = innkeeper::camera::hostCameraPresent();
  LOGI("host camera %s", present ? "present" : "absent");
  auto p = ndk::SharedRefBase::make<innkeeper::camera::Provider>(present);
  const std::string name =
    std::string(innkeeper::camera::Provider::descriptor) + "/innkeeper/0";
  if (
    AServiceManager_addService(p->asBinder().get(), name.c_str()) != STATUS_OK
  ) {
    LOGE("addService %s failed", name.c_str());
    return 1;
  }
  ABinderProcess_joinThreadPool();
  return 1;
}
