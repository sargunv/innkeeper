// SPDX-License-Identifier: AGPL-3.0-or-later
// The host state service's BATTERY event (host/bridge/src/hoststate/proto.rs)
// and what Android's HealthInfo makes of it. Pure C, so it is tested on the
// host (tests/battery_test.c).
#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define INNKEEPER_EV_BATTERY 0x4801
#define INNKEEPER_HOSTSTATE_START 0x0801
#define INNKEEPER_FEATURE_BATTERY 1u

// UPower's view, as the bridge sends it.
struct innkeeper_battery {
  bool present;
  uint8_t state;  // UPower State: 0 unknown .. 6 pending discharge
  bool ac;
  uint8_t level;  // percent
  uint32_t time_to_empty;
  uint32_t time_to_full;  // seconds, 0 unknown
};

// The HealthInfo fields Android reads. Status and capacity level use the
// AIDL enums' values (BatteryStatus, BatteryCapacityLevel).
struct innkeeper_health {
  bool ac_online;
  bool battery_present;
  int32_t status;
  int32_t level;
  int32_t capacity_level;
  int64_t time_to_full_s;  // -1: unsupported
};

// 0, or -EINVAL for a malformed body.
int innkeeper_battery_decode(
  const uint8_t* body, size_t len, struct innkeeper_battery* out
);

// Android's view of `b`. A host without a battery is on AC and full.
void innkeeper_health_from(
  const struct innkeeper_battery* b, struct innkeeper_health* out
);

// What to report before the bridge answers: AC, no battery.
extern const struct innkeeper_battery INNKEEPER_BATTERY_AC_ONLY;

#ifdef __cplusplus
}
#endif
