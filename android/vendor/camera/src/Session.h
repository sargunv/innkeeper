// SPDX-License-Identifier: AGPL-3.0-or-later
// ICameraDeviceSession for the host camera. A worker thread takes capture
// requests in order and fills each output buffer from the latest frame the
// bridge streamed. Until the desktop's first frame arrives, for example while
// the portal's consent dialog is open, frames are black, so cameraserver's
// pipeline keeps moving. If the user denies the camera, or the desktop's
// stream ends after it ran, the session reports ERROR_DEVICE. Metadata travels
// inline, without FMQs.
#pragma once

#include <condition_variable>
#include <deque>
#include <map>
#include <memory>
#include <mutex>
#include <thread>

#include <aidl/android/hardware/camera/device/BnCameraDeviceSession.h>
#include <aidl/android/hardware/camera/device/ICameraDeviceCallback.h>

#include "Buffers.h"
#include "HostCamera.h"
#include "Metadata.h"

namespace innkeeper::camera {

namespace device = aidl::android::hardware::camera::device;

// Whether the HAL can stream `c`: normal mode, sizes and formats from the
// characteristics, at most two YCbCr streams and one JPEG stream.
bool streamsSupported(const device::StreamConfiguration& c);

class Session : public device::BnCameraDeviceSession {
 public:
  explicit Session(std::shared_ptr<device::ICameraDeviceCallback> callback);
  ~Session() override;

  bool isClosed();

  ndk::ScopedAStatus close() override;
  ndk::ScopedAStatus configureStreams(
    const device::StreamConfiguration& config,
    std::vector<device::HalStream>* out
  ) override;
  ndk::ScopedAStatus constructDefaultRequestSettings(
    device::RequestTemplate type, device::CameraMetadata* out
  ) override;
  ndk::ScopedAStatus flush() override;
  ndk::ScopedAStatus getCaptureRequestMetadataQueue(
    aidl::android::hardware::common::fmq::MQDescriptor<
      int8_t, aidl::android::hardware::common::fmq::SynchronizedReadWrite>* out
  ) override;
  ndk::ScopedAStatus getCaptureResultMetadataQueue(
    aidl::android::hardware::common::fmq::MQDescriptor<
      int8_t, aidl::android::hardware::common::fmq::SynchronizedReadWrite>* out
  ) override;
  ndk::ScopedAStatus isReconfigurationRequired(
    const device::CameraMetadata& oldParams,
    const device::CameraMetadata& newParams, bool* out
  ) override;
  ndk::ScopedAStatus processCaptureRequest(
    const std::vector<device::CaptureRequest>& requests,
    const std::vector<device::BufferCache>& cachesToRemove, int32_t* out
  ) override;
  ndk::ScopedAStatus signalStreamFlush(
    const std::vector<int32_t>& streamIds, int32_t counter
  ) override;
  ndk::ScopedAStatus switchToOffline(
    const std::vector<int32_t>& streamsToKeep,
    device::CameraOfflineSessionInfo* info,
    std::shared_ptr<device::ICameraOfflineSession>* out
  ) override;
  ndk::ScopedAStatus repeatingRequestEnd(
    int32_t frameNumber, const std::vector<int32_t>& streamIds
  ) override;

 private:
  // A request the worker owns: its settings and its buffers' acquire fences.
  struct Pending {
    int32_t frameNumber;
    std::vector<uint8_t> settings;
    struct Out {
      int32_t streamId;
      int64_t bufferId;
      int acquireFence = -1;
    };
    std::vector<Out> outputs;
  };

  void work();
  void process(Pending& p);
  void fail(Pending& p, bool device);
  std::shared_ptr<Buffer> buffer(const Pending::Out& o);
  bool fill(
    const device::Stream& s, Buffer& b, const Frame& f, const JpegParams& jpeg
  );
  void closePending(Pending& p);
  // Moves an unwaited acquire fence into the buffer's release fence.
  static void returnFence(Pending::Out& o, device::StreamBuffer* sb);

  std::shared_ptr<device::ICameraDeviceCallback> mCallback;
  HostCamera mHost;
  std::mutex mMutex;
  std::condition_variable mCond;
  std::deque<Pending> mQueue;
  std::map<int32_t, device::Stream> mStreams;
  // Imported buffers by stream and buffer id, under mMutex.
  std::map<std::pair<int32_t, int64_t>, std::shared_ptr<Buffer>> mBuffers;
  std::vector<uint8_t> mSettings;
  bool mClosed = false;
  // The worker is processing a request it took from mQueue.
  bool mBusy = false;
  bool mDeviceError = false;
  std::thread mWorker;
  Frame mFrame;
  // The size the host stream was last started at.
  Size mHostSize{0, 0};
  int64_t mLastNs = 0;
};

}  // namespace innkeeper::camera
