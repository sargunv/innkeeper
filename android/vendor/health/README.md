# health

This directory holds the Health HAL, `android.hardware.health.IHealth/default`
(AIDL V5). `mise run build:ndk` builds it out of tree with the NDK and stages it
as `/vendor/bin/hw/android.hardware.health-service.innkeeper`, with its init rc
and VINTF fragment. It replaces cuttlefish's Health HAL, which fakes a charging
battery. [image/bootconfig](../../../image/bootconfig) drops cuttlefish's health
APEX. With this HAL, `BatteryManager`, `dumpsys battery` and apps see the host's
battery.

- **Source.** The HAL reads `BATTERY` events from the bridge's host state
  service ([hoststate/proto.rs](../../../host/bridge/src/hoststate/proto.rs)),
  which reads UPower's `DisplayDevice`. The session masks host sysfs, so the HAL
  can't read `/sys/class/power_supply`.
- **No battery.** Until the bridge answers, and on a host without a battery, the
  HAL reports AC power, no battery, a level of 100 and a full status. When the
  bridge goes away, the HAL reconnects, as the composer does.
- **Shutdown guards.** BatteryService shuts Android down at level 0 without
  power, so an empty battery on battery power reads 1%. BatteryService also
  shuts down when the battery is hot, so the temperature is a constant 25 °C.
- **Callbacks** follow `hardware/interfaces/health/aidl/default`. A registered
  `IHealthInfoCallback` gets the current `HealthInfo` at once and then every
  change. Storage, disk stats, charging policy and battery health data are
  unsupported.
- The HAL implements the frozen IHealth V5 snapshot in
  `android/vendor/third_party/aosp`. It links the graphics HALs' libbinder_ndk
  stub.

Tests: `tests/battery_test.c` runs in `mise run test` and checks the `BATTERY`
decoder and the `HealthInfo` mapping. Start reading at `src/battery.c`.
