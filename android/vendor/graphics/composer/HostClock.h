// SPDX-License-Identifier: AGPL-3.0-or-later
#pragma once

#include <algorithm>
#include <cstdint>

namespace innkeeper::hwc {

// Host presentation phase in CLOCK_MONOTONIC. Extrapolate between feedback
// samples. Silence on a static visible window preserves its clock; only
// an explicit source removal returns to the boot timer.
class HostClock {
 public:
  static constexpr int64_t kDefaultPeriod = 16'666'666;

  bool update(int64_t timestamp, int64_t period, int64_t now) {
    if (timestamp == 0 && period == 0) {
      mTimestamp = 0;
      mPeriod = kDefaultPeriod;
      return true;
    }
    if (
      timestamp <= 0 || timestamp > now || period < 1'000'000 ||
      period > 100'000'000
    )
      return false;
    mTimestamp = timestamp;
    mPeriod = period;
    return true;
  }
  bool active(int64_t) const { return mTimestamp > 0; }
  int64_t period(int64_t now) const {
    return active(now) ? mPeriod : kDefaultPeriod;
  }
  int64_t next(int64_t now, int64_t last) const {
    const int64_t p = period(now);
    const int64_t phase = active(now) ? mTimestamp : last;
    const int64_t after = std::max(now, last);
    return phase + ((after - phase) / p + 1) * p;
  }

 private:
  int64_t mTimestamp = 0;
  int64_t mPeriod = kDefaultPeriod;
};

}  // namespace innkeeper::hwc
