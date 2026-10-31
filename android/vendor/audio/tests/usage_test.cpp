// SPDX-License-Identifier: AGPL-3.0-only
#include <cstdio>
#include <cstdlib>

#include "Usage.h"

#define CHECK(cond)                                                   \
  do {                                                                \
    if (!(cond)) {                                                    \
      std::fprintf(stderr, "%s:%d: %s\n", __FILE__, __LINE__, #cond); \
      std::abort();                                                   \
    }                                                                 \
  } while (0)

int main() {
  using namespace innkeeper::audio;
  // No tracks: nothing to report, so the caller sends no SET_METADATA.
  CHECK(!selectUsage(nullptr, 0));

  const int32_t media[] = {kUsageMedia};
  CHECK(selectUsage(media, 1) == kUsageMedia);

  // Highest priority wins, independent of track order.
  const int32_t mixed[] = {
    kUsageNotification, kUsageMedia, kUsageGame, kUsageAlarm
  };
  CHECK(selectUsage(mixed, 4) == kUsageGame);
  const int32_t call[] = {kUsageMedia, kUsageVoiceCommunication, kUsageGame};
  CHECK(selectUsage(call, 3) == kUsageVoiceCommunication);
  const int32_t signalling[] = {
    kUsageAssistant, kUsageVoiceCommunicationSignalling
  };
  CHECK(selectUsage(signalling, 2) == kUsageVoiceCommunicationSignalling);
  const int32_t callAssistant[] = {kUsageMedia, kUsageCallAssistant};
  CHECK(selectUsage(callAssistant, 2) == kUsageCallAssistant);
  const int32_t nav[] = {
    kUsageNotification, kUsageAssistanceNavigationGuidance
  };
  CHECK(selectUsage(nav, 2) == kUsageAssistanceNavigationGuidance);
  const int32_t assistant[] = {kUsageAssistanceSonification, kUsageAssistant};
  CHECK(selectUsage(assistant, 2) == kUsageAssistant);
  const int32_t system[] = {kUsageUnknown, kUsageAssistanceAccessibility};
  CHECK(selectUsage(system, 2) == kUsageAssistanceAccessibility);
  const int32_t alarm[] = {kUsageUnknown, kUsageAlarm};
  CHECK(selectUsage(alarm, 2) == kUsageAlarm);

  // An unknown usage loses to any known track and is kept when it is alone.
  const int32_t unknown = 1000;  // EMERGENCY, unlisted
  const int32_t withUnknown[] = {unknown, kUsageMedia};
  CHECK(selectUsage(withUnknown, 2) == kUsageMedia);
  CHECK(selectUsage(&unknown, 1) == unknown);
  const int32_t invalid = -1;
  CHECK(selectUsage(&invalid, 1) == invalid);

  // The first track wins a tie, so a repeated update of the same set reports
  // the same usage and the caller does not resend.
  const int32_t tie[] = {
    kUsageNotification, kUsageAlarm, kUsageNotificationRingtone
  };
  auto first = selectUsage(tie, 3);
  auto again = selectUsage(tie, 3);
  CHECK(first == kUsageNotification);
  CHECK(again == first);
}
