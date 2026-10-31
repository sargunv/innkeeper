// SPDX-License-Identifier: AGPL-3.0-or-later
#include <errno.h>

#include "battery.h"

// android.hardware.health BatteryStatus and BatteryCapacityLevel.
enum {
  ST_UNKNOWN = 1,
  ST_CHARGING = 2,
  ST_DISCHARGING = 3,
  ST_NOT_CHARGING = 4,
  ST_FULL = 5
};
enum {
  CAP_CRITICAL = 1,
  CAP_LOW = 2,
  CAP_NORMAL = 3,
  CAP_HIGH = 4,
  CAP_FULL = 5
};

const struct innkeeper_battery INNKEEPER_BATTERY_AC_ONLY = {
  .present = false,
  .state = 0,
  .ac = true,
  .level = 100,
};

static uint32_t u32le(const uint8_t* p) {
  return (uint32_t)p[0] | (uint32_t)p[1] << 8 | (uint32_t)p[2] << 16 |
         (uint32_t)p[3] << 24;
}

int innkeeper_battery_decode(
  const uint8_t* b, size_t len, struct innkeeper_battery* out
) {
  if (len != 12 || b[0] > 1 || b[1] > 6 || b[2] > 1 || b[3] > 100)
    return -EINVAL;
  out->present = b[0];
  out->state = b[1];
  out->ac = b[2];
  out->level = b[3];
  out->time_to_empty = u32le(b + 4);
  out->time_to_full = u32le(b + 8);
  return 0;
}

void innkeeper_health_from(
  const struct innkeeper_battery* b, struct innkeeper_health* h
) {
  if (!b->present) {
    *h = (struct innkeeper_health){
      .ac_online = true,
      .battery_present = false,
      .status = ST_FULL,
      .level = 100,
      .capacity_level = CAP_FULL,
      .time_to_full_s = -1,
    };
    return;
  }
  int status;
  switch (b->state) {
    case 1:
      status = ST_CHARGING;
      break;
    case 2:
    case 3:
    case 6:
      status = ST_DISCHARGING;
      break;
    case 4:
      status = ST_FULL;
      break;
    case 5:
      status = ST_NOT_CHARGING;
      break;
    default:
      status = ST_UNKNOWN;
      break;
  }
  const bool ac = b->ac || status == ST_CHARGING || status == ST_FULL;
  int level = b->level;
  // BatteryService shuts Android down at level 0 without power: a session
  // never does that to the desktop it runs in.
  if (level < 1 && !ac) level = 1;
  int cap;
  if (status == ST_FULL || level >= 100)
    cap = CAP_FULL;
  else if (level >= 80)
    cap = CAP_HIGH;
  else if (level > 15)
    cap = CAP_NORMAL;
  else if (level > 5)
    cap = CAP_LOW;
  else
    cap = CAP_CRITICAL;
  *h = (struct innkeeper_health){
    .ac_online = ac,
    .battery_present = true,
    .status = status,
    .level = level,
    .capacity_level = cap,
    .time_to_full_s =
      status == ST_CHARGING && b->time_to_full ? (int64_t)b->time_to_full : -1,
  };
}
