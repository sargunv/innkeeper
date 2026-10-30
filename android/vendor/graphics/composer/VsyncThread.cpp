// SPDX-License-Identifier: AGPL-3.0-or-later
#define LOG_TAG "linuxapk-hwc"
#include <pthread.h>
#include <sched.h>

#include "VsyncThread.h"

#include "Common.h"
#include "HostClock.h"
#include "Properties.h"
#include "linuxapk/log.h"

namespace linuxapk::hwc {

namespace {
std::mutex gClockMutex;
HostClock gClock;
}  // namespace

void VsyncThread::hostTiming(int64_t timestampNs, int64_t periodNs) {
  std::lock_guard lock(gClockMutex);
  gClock.update(timestampNs, periodNs, nowNs());
}

VsyncThread::VsyncThread(int64_t displayId, int64_t periodNs)
    : mDisplayId(displayId), mPeriodNs(periodNs) {
  mThread = std::thread([this] { loop(); });
}

VsyncThread::~VsyncThread() {
  {
    std::lock_guard lock(mMutex);
    mStop = true;
  }
  mCv.notify_all();
  mThread.join();
}

void VsyncThread::setCallback(
  std::shared_ptr<
    ::aidl::android::hardware::graphics::composer3::IComposerCallback>
    cb
) {
  std::lock_guard lock(mMutex);
  mCallback = std::move(cb);
}

void VsyncThread::setEnabled(bool enabled) {
  mEnabled = enabled;
  mCv.notify_all();
}

void VsyncThread::loop() {
  pthread_setname_np(pthread_self(), "hwc-vsync");
  struct sched_param param = {.sched_priority = 2};
  sched_setscheduler(
    0, SCHED_FIFO | SCHED_RESET_ON_FORK, &param
  );  // best effort

  int64_t next = nowNs();
  std::unique_lock lock(mMutex);
  while (!mStop) {
    const int64_t now = nowNs();
    int64_t period = mPeriodNs.load();
    if (
      mDisplayId == 0 && getProperty("vendor.linuxapk.hwc.host_clock") != "0"
    ) {
      std::lock_guard clockLock(gClockMutex);
      next = gClock.next(now, mLastVsyncNs.load());
      period = gClock.period(now);
    } else {
      next += period;
      if (next <= now) next += ((now - next) / period + 1) * period;
    }
    const bool changed = period != mPeriodNs.exchange(period);
    if (
      mCv.wait_for(
        lock, std::chrono::nanoseconds(std::max(int64_t(0), next - nowNs())),
        [this] { return mStop; }
      )
    )
      break;
    mLastVsyncNs = next;
    if (mStop) break;
    if (mCallback && (mEnabled || changed)) {
      auto cb = mCallback;
      lock.unlock();
      // Re-enumeration updates SF's mode period as well as its samples.
      if (changed)
        cb->onHotplugEvent(
          mDisplayId, ::aidl::android::hardware::graphics::common::
                        DisplayHotplugEvent::CONNECTED
        );
      if (mEnabled && getProperty("vendor.linuxapk.hwc.bench") == "1")
        LOGI(
          "BENCH vsync display=%lld ns=%lld period_ns=%lld",
          (long long)mDisplayId, (long long)next, (long long)period
        );
      auto st = mEnabled ? cb->onVsync(mDisplayId, next, int32_t(period))
                         : ndk::ScopedAStatus::ok();
      if (!st.isOk()) LOGW("onVsync failed: %s", st.getDescription().c_str());
      lock.lock();
    }
  }
}

}  // namespace linuxapk::hwc
