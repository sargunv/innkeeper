// SPDX-License-Identifier: AGPL-3.0-or-later
// Decodes the host state service's BATTERY event
// (host/bridge/src/hoststate/proto.rs) and maps it to Android's HealthInfo. It
// is plain C, so it is tested on the host (tests/battery_test.c).
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

// The battery state from UPower, as the bridge sends it.
struct innkeeper_battery {
  bool present;
  uint8_t state;  // UPower State: 0 unknown .. 6 pending discharge
  bool ac;
  uint8_t level;  // percent
  uint32_t time_to_empty;
  uint32_t time_to_full;  // seconds, or 0 if unknown
};

// The HealthInfo fields Android reads. Status and capacity level use the
// AIDL enums' values (BatteryStatus, BatteryCapacityLevel).
struct innkeeper_health {
  bool ac_online;
  bool battery_present;
  int32_t status;
  int32_t level;
  int32_t capacity_level;
  int64_t time_to_full_s;  // -1 if unsupported
};

// Returns 0, or -EINVAL for a malformed body.
int innkeeper_battery_decode(
  const uint8_t* body, size_t len, struct innkeeper_battery* out
);

// Maps `b` to Android's HealthInfo. A host without a battery reports AC power
// and a full battery.
void innkeeper_health_from(
  const struct innkeeper_battery* b, struct innkeeper_health* out
);

// The state reported before the bridge answers, with AC power and no
// battery.
extern const struct innkeeper_battery INNKEEPER_BATTERY_AC_ONLY;

#ifdef __cplusplus
}
#endif
