// SPDX-License-Identifier: AGPL-3.0-or-later
// The BATTERY event as host/bridge/src/hoststate/proto.rs encodes it, and
// the HealthInfo fields the Health HAL reports for it.
#include <assert.h>
#include <stdio.h>
#include <string.h>

#include "battery.h"

int main(void) {
  // present, discharging, on battery, 42%, 1 h to empty (Battery::encode).
  const uint8_t ev[12] = {1, 2, 0, 42, 0x10, 0x0e, 0, 0, 0, 0, 0, 0};
  struct innkeeper_battery b;
  assert(innkeeper_battery_decode(ev, sizeof ev, &b) == 0);
  assert(
    b.present && b.state == 2 && !b.ac && b.level == 42 &&
    b.time_to_empty == 3600
  );
  struct innkeeper_health h;
  innkeeper_health_from(&b, &h);
  assert(h.battery_present && !h.ac_online && h.status == 3 && h.level == 42);
  assert(h.capacity_level == 3 && h.time_to_full_s == -1);

  // Charging: on AC whatever OnBattery said, with a time to full.
  b.state = 1;
  b.time_to_full = 600;
  innkeeper_health_from(&b, &h);
  assert(h.ac_online && h.status == 2 && h.time_to_full_s == 600);

  // No battery: AC and full.
  innkeeper_health_from(&INNKEEPER_BATTERY_AC_ONLY, &h);
  assert(h.ac_online && !h.battery_present && h.status == 5 && h.level == 100);

  // Empty and unplugged never reads 0 (BatteryService would shut down).
  b = (struct innkeeper_battery){
    .present = true, .state = 3, .ac = false, .level = 0
  };
  innkeeper_health_from(&b, &h);
  assert(h.level == 1 && h.status == 3 && h.capacity_level == 1);

  // Malformed bodies.
  uint8_t bad[12];
  memcpy(bad, ev, sizeof bad);
  bad[3] = 101;
  assert(innkeeper_battery_decode(bad, sizeof bad, &b) < 0);
  bad[3] = 42;
  bad[1] = 7;
  assert(innkeeper_battery_decode(bad, sizeof bad, &b) < 0);
  assert(innkeeper_battery_decode(ev, 11, &b) < 0);
  puts("battery_test: ok");
  return 0;
}
