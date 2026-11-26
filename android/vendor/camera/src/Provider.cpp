// SPDX-License-Identifier: AGPL-3.0-or-later
#define LOG_TAG "innkeeper-camera"

#include "Provider.h"

#include "Metadata.h"
#include "Session.h"
#include "innkeeper/log.h"

namespace innkeeper::camera {

using ndk::ScopedAStatus;

namespace {

// android.hardware.camera.common.Status.
constexpr int32_t kIllegalArgument = 1, kCameraInUse = 2,
                  kOperationNotSupported = 4;

ScopedAStatus error(int32_t status) {
  return ScopedAStatus::fromServiceSpecificError(status);
}

}  // namespace

ScopedAStatus Device::getCameraCharacteristics(device::CameraMetadata* out) {
  out->metadata = characteristics().serialize();
  return ScopedAStatus::ok();
}

ScopedAStatus Device::getPhysicalCameraCharacteristics(
  const std::string&, device::CameraMetadata*
) {
  return error(kIllegalArgument);
}

ScopedAStatus Device::getResourceCost(common::CameraResourceCost* out) {
  out->resourceCost = 100;
  return ScopedAStatus::ok();
}

ScopedAStatus Device::isStreamCombinationSupported(
  const device::StreamConfiguration& streams, bool* out
) {
  *out = streamsSupported(streams);
  return ScopedAStatus::ok();
}

ScopedAStatus Device::open(
  const std::shared_ptr<device::ICameraDeviceCallback>& callback,
  std::shared_ptr<device::ICameraDeviceSession>* out
) {
  if (!callback) return error(kIllegalArgument);
  std::lock_guard lock(mMutex);
  // cameraserver closes a session before it opens the next one.
  if (mSession.lock()) return error(kCameraInUse);
  auto s = ndk::SharedRefBase::make<Session>(callback);
  mSession = s;
  *out = s;
  LOGI("session opened");
  return ScopedAStatus::ok();
}

ScopedAStatus Device::openInjectionSession(
  const std::shared_ptr<device::ICameraDeviceCallback>&,
  std::shared_ptr<device::ICameraInjectionSession>*
) {
  return error(kOperationNotSupported);
}

ScopedAStatus Device::setTorchMode(bool) {
  return error(kOperationNotSupported);
}

ScopedAStatus Device::turnOnTorchWithStrengthLevel(int32_t) {
  return error(kOperationNotSupported);
}

ScopedAStatus Device::getTorchStrengthLevel(int32_t*) {
  return error(kOperationNotSupported);
}

Provider::Provider(bool present)
    : mPresent(present), mDevice(ndk::SharedRefBase::make<Device>()) {}

ScopedAStatus Provider::setCallback(
  const std::shared_ptr<provider::ICameraProviderCallback>&
) {
  return ScopedAStatus::ok();
}

ScopedAStatus Provider::getVendorTags(
  std::vector<common::VendorTagSection>* out
) {
  out->clear();
  return ScopedAStatus::ok();
}

ScopedAStatus Provider::getCameraIdList(std::vector<std::string>* out) {
  out->clear();
  if (mPresent) out->push_back(kDeviceName);
  return ScopedAStatus::ok();
}

ScopedAStatus Provider::getCameraDeviceInterface(
  const std::string& name, std::shared_ptr<device::ICameraDevice>* out
) {
  if (!mPresent || name != kDeviceName) return error(kIllegalArgument);
  *out = mDevice;
  return ScopedAStatus::ok();
}

ScopedAStatus Provider::notifyDeviceStateChange(int64_t) {
  return ScopedAStatus::ok();
}

ScopedAStatus Provider::getConcurrentCameraIds(
  std::vector<provider::ConcurrentCameraIdCombination>* out
) {
  out->clear();
  return ScopedAStatus::ok();
}

ScopedAStatus Provider::isConcurrentStreamCombinationSupported(
  const std::vector<provider::CameraIdAndStreamCombination>&, bool* out
) {
  *out = false;
  return ScopedAStatus::ok();
}

}  // namespace innkeeper::camera
