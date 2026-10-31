# health

The Health HAL, `android.hardware.health.IHealth/default` (AIDL V5), built out
of tree with the NDK by `mise run build:ndk` and staged as
`/vendor/bin/hw/android.hardware.health-service.innkeeper` with its init rc and
VINTF fragment. It replaces cuttlefish's (which fakes a charging battery; its
APEX is dropped in [image/bootconfig](../../../image/bootconfig)), so
`BatteryManager`, `dumpsys battery` and apps see the host's battery.

- **Source:** the bridge's host state service (`BATTERY` events,
  [hoststate/proto.rs](../../../host/bridge/src/hoststate/proto.rs)), which
  reads UPower's `DisplayDevice`. The session masks host sysfs, so the HAL never
  reads `/sys/class/power_supply`.
- **Before the bridge answers,** and on a host without a battery, it reports AC
  power, no battery, level 100 and full. It reconnects whenever the bridge goes
  away, as the composer does.
- **Never level 0 unplugged:** BatteryService shuts Android down at level 0
  without power, so an empty battery reads 1%. The temperature is a constant 25
  °C for the same reason.
- **Callbacks** follow `hardware/interfaces/health/aidl/default`: a registered
  `IHealthInfoCallback` gets the current `HealthInfo` at once, then every
  change. Storage, disk stats, charging policy and battery health data are
  unsupported.
- It implements the frozen IHealth V5 snapshot in
  `android/vendor/third_party/aosp`, and links the graphics HALs' libbinder_ndk
  stub.

Tests: `tests/battery_test.c` (in `mise run test`) checks the `BATTERY` decoder
and the `HealthInfo` mapping. Start reading at `src/battery.c`.
