// SPDX-License-Identifier: AGPL-3.0-or-later
#define LOG_TAG "linuxapk-hwc"
#include <pthread.h>
#include <sched.h>

#include "VsyncThread.h"

#include "Common.h"
#include "linuxapk/log.h"

namespace linuxapk::hwc {

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

void VsyncThread::setPeriod(int64_t periodNs) { mPeriodNs = periodNs; }

void VsyncThread::loop() {
  pthread_setname_np(pthread_self(), "hwc-vsync");
  struct sched_param param = {.sched_priority = 2};
  sched_setscheduler(
    0, SCHED_FIFO | SCHED_RESET_ON_FORK, &param
  );  // best effort

  int64_t next = nowNs();
  std::unique_lock lock(mMutex);
  while (!mStop) {
    const int64_t period = mPeriodNs.load();
    next += period;
    const int64_t now = nowNs();
    if (next < now)
      next = now + period - (now - next) % period;  // skip missed ticks
    lock.unlock();
    timespec ts = {time_t(next / 1000000000), long(next % 1000000000)};
    while (clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &ts, nullptr) ==
           EINTR) {
    }
    lock.lock();
    mLastVsyncNs = next;
    if (mStop) break;
    if (mEnabled && mCallback) {
      auto cb = mCallback;
      lock.unlock();
      auto st = cb->onVsync(mDisplayId, next, int32_t(period));
      if (!st.isOk()) LOGW("onVsync failed: %s", st.getDescription().c_str());
      lock.lock();
    }
  }
}

}  // namespace linuxapk::hwc
