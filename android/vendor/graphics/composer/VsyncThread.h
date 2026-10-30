// SPDX-License-Identifier: AGPL-3.0-or-later
// Primary scheduling clock follows host presentation phase; other displays
// and the no-window fallback use a CLOCK_MONOTONIC timer.
#pragma once

#include <atomic>
#include <condition_variable>
#include <memory>
#include <mutex>
#include <thread>

#include <aidl/android/hardware/graphics/composer3/IComposerCallback.h>

namespace linuxapk::hwc {

class VsyncThread {
 public:
  VsyncThread(int64_t displayId, int64_t periodNs);
  ~VsyncThread();

  void setCallback(
    std::shared_ptr<
      ::aidl::android::hardware::graphics::composer3::IComposerCallback>
  );
  void setEnabled(bool enabled);
  int64_t period() const { return mPeriodNs.load(); }
  static void hostTiming(int64_t timestampNs, int64_t periodNs);
  // Most recent vsync timestamp (whether or not it was delivered).
  int64_t lastVsyncNs() const { return mLastVsyncNs.load(); }

 private:
  void loop();

  const int64_t mDisplayId;
  std::atomic<int64_t> mPeriodNs;
  std::atomic<int64_t> mLastVsyncNs{0};
  std::atomic<bool> mEnabled{false};
  std::mutex mMutex;
  std::condition_variable mCv;
  bool mStop = false;
  std::shared_ptr<
    ::aidl::android::hardware::graphics::composer3::IComposerCallback>
    mCallback;
  std::thread mThread;
};

}  // namespace linuxapk::hwc
