// SPDX-License-Identifier: AGPL-3.0-or-later
// The camera HAL's side of the bridge's camera service
// (host/bridge/src/camera/proto.rs). present() asks whether the desktop has a
// camera. A HostCamera streams it: start() sends START and maps the frame
// ring read-only, and a reader thread follows STATUS and FRAME events.
#pragma once

#include <condition_variable>
#include <cstdint>
#include <mutex>
#include <thread>
#include <vector>

namespace innkeeper::camera {

// The desktop's camera, through the Camera portal. False when the bridge is
// absent or doesn't answer.
bool hostCameraPresent();

// One NV12 frame, tightly packed.
struct Frame {
  uint32_t width = 0, height = 0;
  uint64_t number = 0;
  int64_t timestampNs = 0;
  std::vector<uint8_t> data;
};

class HostCamera {
 public:
  // Lost: the stream ran, then ended, as when the camera is unplugged.
  enum class State { Starting, Running, Denied, Ended, Lost };

  ~HostCamera();

  // Starts streaming near width x height. Returns 0, -EACCES if the user
  // denied the camera earlier in the session, or another -errno.
  int start(uint32_t width, uint32_t height);
  void stop();

  // Waits up to timeoutMs for a frame newer than `after` and copies it into
  // *out. Returns false on timeout or when the stream is over.
  bool next(uint64_t after, int timeoutMs, Frame* out);
  State state();

 private:
  void readEvents();
  bool copySlot(uint32_t slot, Frame* out);

  int mSock = -1;
  const uint8_t* mRing = nullptr;
  size_t mRingSize = 0;
  uint32_t mSlots = 0, mSlotBytes = 0;
  std::thread mReader;
  std::mutex mMutex;
  std::condition_variable mCond;
  State mState = State::Ended;
  uint32_t mLatestSlot = 0;
  uint64_t mLatest = 0;
};

}  // namespace innkeeper::camera
