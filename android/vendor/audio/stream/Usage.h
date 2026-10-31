// SPDX-License-Identifier: AGPL-3.0-only
#pragma once
#include <cstddef>
#include <cstdint>
#include <optional>

namespace linuxapk::audio {
// Android audio_usage_t / AudioUsage values the bridge maps to a mixer name.
// Mirrors android.media.audio.common.AudioUsage; kept as integers so host
// tests can compile this header without the generated AIDL bindings.
inline constexpr int32_t kUsageUnknown = 0;
inline constexpr int32_t kUsageMedia = 1;
inline constexpr int32_t kUsageVoiceCommunication = 2;
inline constexpr int32_t kUsageVoiceCommunicationSignalling = 3;
inline constexpr int32_t kUsageAlarm = 4;
inline constexpr int32_t kUsageNotification = 5;
inline constexpr int32_t kUsageNotificationRingtone = 6;
inline constexpr int32_t kUsageNotificationCommunicationRequest = 7;
inline constexpr int32_t kUsageNotificationCommunicationInstant = 8;
inline constexpr int32_t kUsageNotificationCommunicationDelayed = 9;
inline constexpr int32_t kUsageNotificationEvent = 10;
inline constexpr int32_t kUsageAssistanceAccessibility = 11;
inline constexpr int32_t kUsageAssistanceNavigationGuidance = 12;
inline constexpr int32_t kUsageAssistanceSonification = 13;
inline constexpr int32_t kUsageGame = 14;
inline constexpr int32_t kUsageAssistant = 16;
inline constexpr int32_t kUsageCallAssistant = 17;

// Priority of an active playback track, highest first:
// voice call > game > media > assistant > navigation > notification/alarm >
// system sounds > unknown. Unlisted usages (emergency, virtual source, …)
// rank as unknown, so a known track still wins.
inline int usagePriority(int32_t usage) {
  switch (usage) {
    case kUsageVoiceCommunication:
    case kUsageVoiceCommunicationSignalling:
    case kUsageCallAssistant:
      return 7;
    case kUsageGame:
      return 6;
    case kUsageMedia:
      return 5;
    case kUsageAssistant:
      return 4;
    case kUsageAssistanceNavigationGuidance:
      return 3;
    case kUsageAlarm:
    case kUsageNotification:
    case kUsageNotificationRingtone:
    case kUsageNotificationCommunicationRequest:
    case kUsageNotificationCommunicationInstant:
    case kUsageNotificationCommunicationDelayed:
    case kUsageNotificationEvent:
      return 2;
    case kUsageAssistanceAccessibility:
    case kUsageAssistanceSonification:
      return 1;
    default:
      return 0;
  }
}

// Highest-priority usage among the active tracks. No tracks: nothing to
// report (the caller sends no SET_METADATA).
inline std::optional<int32_t> selectUsage(const int32_t* usages, size_t count) {
  if (count == 0) return std::nullopt;
  int32_t best = usages[0];
  int rank = usagePriority(best);
  for (size_t i = 1; i < count; i++) {
    int next = usagePriority(usages[i]);
    if (next > rank) {
      best = usages[i];
      rank = next;
    }
  }
  return best;
}

// Sends SET_METADATA with an empty label only when the chosen usage changes.
// No tracks sends nothing. `reported` is the usage last sent, if any.
template <class Send>
inline void reportUsage(
  const int32_t* usages, size_t count, std::optional<int32_t>* reported,
  Send&& send
) {
  auto chosen = selectUsage(usages, count);
  if (!chosen || *reported == chosen) return;
  *reported = chosen;
  send(*chosen);
}
}  // namespace linuxapk::audio
