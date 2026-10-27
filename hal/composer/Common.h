// SPDX-License-Identifier: AGPL-3.0-or-later
#pragma once

#include <cstdint>
#include <ctime>

#include <aidl/android/hardware/graphics/composer3/IComposerClient.h>
#include <android/binder_auto_utils.h>

namespace linuxapk::hwc {

namespace c3 = ::aidl::android::hardware::graphics::composer3;
namespace common = ::aidl::android::hardware::graphics::common;

// HWC3 error codes (IComposerClient::EX_*), used both as service-specific
// binder errors and as CommandError.errorCode.
enum class Error : int32_t {
  None = 0,
  BadConfig = c3::IComposerClient::EX_BAD_CONFIG,
  BadDisplay = c3::IComposerClient::EX_BAD_DISPLAY,
  BadLayer = c3::IComposerClient::EX_BAD_LAYER,
  BadParameter = c3::IComposerClient::EX_BAD_PARAMETER,
  NoResources = c3::IComposerClient::EX_NO_RESOURCES,
  NotValidated = c3::IComposerClient::EX_NOT_VALIDATED,
  Unsupported = c3::IComposerClient::EX_UNSUPPORTED,
  SeamlessNotAllowed = c3::IComposerClient::EX_SEAMLESS_NOT_ALLOWED,
};

inline ndk::ScopedAStatus toStatus(Error e) {
  return e == Error::None
           ? ndk::ScopedAStatus::ok()
           : ndk::ScopedAStatus::fromServiceSpecificError(int32_t(e));
}

inline int64_t nowNs() {
  timespec ts;
  clock_gettime(CLOCK_MONOTONIC, &ts);
  return int64_t(ts.tv_sec) * 1000000000 + ts.tv_nsec;
}

}  // namespace linuxapk::hwc
