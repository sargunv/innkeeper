// SPDX-License-Identifier: AGPL-3.0-only
#pragma once
#include <algorithm>
#include <cstdint>

namespace innkeeper::audio {
inline constexpr uint32_t kRate = 48000;
inline constexpr uint32_t kChannels = 2;
inline constexpr uint32_t kFrameBytes = kChannels * sizeof(int16_t);
inline constexpr uint32_t kPeriodFrames = kRate / 200;  // 5 ms for FastMixer
inline constexpr uint32_t kPeriods = 2;
// Two ring periods and the bridge's 20 ms downstream request.
inline constexpr int32_t kNominalLatencyMs = 30;
inline bool validBufferSize(int64_t frames) {
  return frames >= kPeriodFrames && frames <= kRate;
}
inline uint64_t presentedFrames(uint64_t written, uint64_t queued) {
  return written - std::min(written, queued);
}
inline int32_t latencyMs(uint64_t queued) {
  return (queued * 1000 + kRate - 1) / kRate;
}
}  // namespace innkeeper::audio
