// SPDX-License-Identifier: AGPL-3.0-or-later
// android.hardware.camera.provider.ICameraProvider/innkeeper/0 (AIDL V1). It
// has one device, the desktop's camera, when the bridge's camera service
// reports one. The device's ICameraDevice opens a Session.
#pragma once

#include <memory>
#include <mutex>

#include <aidl/android/hardware/camera/device/BnCameraDevice.h>
#include <aidl/android/hardware/camera/provider/BnCameraProvider.h>

namespace innkeeper::camera {

class Session;

namespace device = aidl::android::hardware::camera::device;
namespace provider = aidl::android::hardware::camera::provider;
namespace common = aidl::android::hardware::camera::common;

// The camera's name: device@<AIDL device major.minor>/<provider type>/<id>.
inline constexpr char kDeviceName[] = "device@1.1/innkeeper/0";

class Device : public device::BnCameraDevice {
 public:
  ndk::ScopedAStatus getCameraCharacteristics(
    device::CameraMetadata* out
  ) override;
  ndk::ScopedAStatus getPhysicalCameraCharacteristics(
    const std::string& id, device::CameraMetadata* out
  ) override;
  ndk::ScopedAStatus getResourceCost(common::CameraResourceCost* out) override;
  ndk::ScopedAStatus isStreamCombinationSupported(
    const device::StreamConfiguration& streams, bool* out
  ) override;
  ndk::ScopedAStatus open(
    const std::shared_ptr<device::ICameraDeviceCallback>& callback,
    std::shared_ptr<device::ICameraDeviceSession>* out
  ) override;
  ndk::ScopedAStatus openInjectionSession(
    const std::shared_ptr<device::ICameraDeviceCallback>& callback,
    std::shared_ptr<device::ICameraInjectionSession>* out
  ) override;
  ndk::ScopedAStatus setTorchMode(bool on) override;
  ndk::ScopedAStatus turnOnTorchWithStrengthLevel(int32_t level) override;
  ndk::ScopedAStatus getTorchStrengthLevel(int32_t* out) override;

 private:
  std::mutex mMutex;
  std::weak_ptr<Session> mSession;
};

class Provider : public provider::BnCameraProvider {
 public:
  explicit Provider(bool present);

  ndk::ScopedAStatus setCallback(
    const std::shared_ptr<provider::ICameraProviderCallback>& callback
  ) override;
  ndk::ScopedAStatus getVendorTags(
    std::vector<common::VendorTagSection>* out
  ) override;
  ndk::ScopedAStatus getCameraIdList(std::vector<std::string>* out) override;
  ndk::ScopedAStatus getCameraDeviceInterface(
    const std::string& name, std::shared_ptr<device::ICameraDevice>* out
  ) override;
  ndk::ScopedAStatus notifyDeviceStateChange(int64_t state) override;
  ndk::ScopedAStatus getConcurrentCameraIds(
    std::vector<provider::ConcurrentCameraIdCombination>* out
  ) override;
  ndk::ScopedAStatus isConcurrentStreamCombinationSupported(
    const std::vector<provider::CameraIdAndStreamCombination>& configs,
    bool* out
  ) override;

 private:
  const bool mPresent;
  std::shared_ptr<Device> mDevice;
};

}  // namespace innkeeper::camera
