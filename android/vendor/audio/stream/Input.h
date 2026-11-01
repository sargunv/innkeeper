// SPDX-License-Identifier: AGPL-3.0-or-later
#pragma once
#include <cstddef>
#include <cstdint>

namespace innkeeper::audio::input {
inline constexpr uint32_t kRate = 48000;
inline constexpr uint32_t kPeriodFrames = 480;  // 10 ms capture bursts
// The source cannot backpressure, so the ring retains 80 ms to absorb
// scheduling jitter. A read is served without waiting for the ring to fill.
inline constexpr uint32_t kPeriods = 8;
inline constexpr int32_t kNominalLatencyMs = 10;
inline bool validBufferSize(int64_t frames) {
  return frames >= kPeriodFrames && frames <= kRate;
}
inline bool validBurst(int32_t bytes, uint32_t frameBytes, size_t space) {
  return bytes >= 0 && bytes % frameBytes == 0 &&
         static_cast<size_t>(bytes) <= space;
}
}  // namespace innkeeper::audio::input
