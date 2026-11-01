// SPDX-License-Identifier: AGPL-3.0-or-later
#pragma once
#include <android/binder_interface_utils.h>

namespace innkeeper::audio {
// AIDL's generated delegator supplies the optional operations from its default
// implementation. Advertise the same frozen version as the public binder.
template <class Default, class Interface>
struct Defaults : Default {
  ndk::ScopedAStatus getInterfaceVersion(int32_t* version) override {
    *version = Interface::version;
    return ndk::ScopedAStatus::ok();
  }
};
inline ndk::ScopedAStatus invalid() {
  return ndk::ScopedAStatus::fromExceptionCode(EX_ILLEGAL_ARGUMENT);
}
inline ndk::ScopedAStatus unsupported() {
  return ndk::ScopedAStatus::fromExceptionCode(EX_UNSUPPORTED_OPERATION);
}
}  // namespace innkeeper::audio
