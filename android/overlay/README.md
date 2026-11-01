# overlay

This directory is the checked-in config layer over the image's partitions, with
one directory per partition. `mise run build:data` copies it into the stage's
`share/innkeeper/android/`, and the session stacks that above the image
([docs/session.md](../../docs/session.md#overlay-stack)). A zero-byte file or an
empty manifest blanks the image's file at the same path.

## Forked files

These are copies of image files with our edits, and each edit is marked
`innkeeper:`. The upstream base hashes in
[image/fork-bases.sha256](../../image/fork-bases.sha256) guard them. If the
image's copies change, `prepare` fails and prints the image-to-overlay diffs.
When the pin moves, port those changes before refreshing the hashes
([image/README.md](../../image/README.md#bumping-the-pin)).

Services aren't forked. `system_ext/etc/init/innkeeper-services.rc` replaces the
definitions of servicemanager, vold, installd, netd, netd1shot, zygote and lmkd
with `override` stanzas. The stanzas add the shim and its role, and make netd's
self test a no-op. `fork-bases.sha256` also guards the source files of these
services.

| File                                             | Why                                                                                                                                            |
| ------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| `system/etc/init/hw/init.rc`                     | no ueventd (`ro.cold_boot_done` set directly), `sys.use_memfd` (no ashmem), the flattened `/apex` bind instead of apexd-bootstrap, no binderfs |
| `system/etc/init/apexd.rc`                       | runs `innkeeper-apexd`, which shows apexd the pre-mounted APEXes; forked because an override can't redeclare its `interface`                   |
| `system/etc/init/hw/init.usb.rc`                 | adbd doesn't start from the USB configuration; `init.innkeeper.rc` starts it for a profile with adb on                                         |
| `system/etc/cgroups.json`                        | cgroup2 only, because the session has no v1 hierarchies                                                                                        |
| `apex/com.android.tethering/etc/netbpfload.35rc` | runs netbpfload through `innkeeper-bpfload` against the emulated `bpf()`                                                                       |

## Added files

| File                                                        | Why                                                                                                |
| ----------------------------------------------------------- | -------------------------------------------------------------------------------------------------- |
| `vendor/etc/init/hw/init.innkeeper.rc`                      | the device rc for `androidboot.hardware=innkeeper`, in place of cuttlefish's; adb, navigation mode |
| `system/bin/innkeeper-apexd`, `innkeeper-bpfload`           | wrappers with private mount namespaces (see their headers)                                         |
| `vendor/etc/permissions/innkeeper-unavailable-hardware.xml` | no Bluetooth, because without its HAL `com.android.bluetooth` aborts in a loop                     |
| `system_ext/etc/init/innkeeper-services.rc`                 | override stanzas for the services above                                                            |

## Blanked files

| File                                                                                                                              | What it was                                                                                                    |
| --------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------- |
| `vendor/etc/init/init.cutf_cvm.rc`                                                                                                | cuttlefish's device rc: VM kernel modules, fstab, adb over vsock                                               |
| `vendor/etc/init/seriallogging.rc`                                                                                                | `logcat -b all` to a virtio console                                                                            |
| `system/etc/init/usbd.rc`                                                                                                         | `usbd`, which starts adbd at boot when `persist.sys.usb.config` has adb (the image's does); no USB gadget here |
| `system_ext/etc/init/rpmb_dev.test.system.rc`                                                                                     | an RPMB mock for Trusty test VMs                                                                               |
| `product/etc/init/init.rc`                                                                                                        | `tombstone_transmit` over vsock                                                                                |
| `vendor/etc/init/android.hardware.sensors-service-multihal.rc`, `vendor/etc/vintf/manifest/android.hardware.sensors-multihal.xml` | the sensors HAL; the session has no sensors                                                                    |
| `vendor/etc/vintf/manifest/{face,fingerprint}-default.xml`                                                                        | biometrics HALs that are declared but don't start                                                              |
| `vendor/etc/vintf/manifest/mapper.minigbm.xml`                                                                                    | the minigbm mapper; ours replaces it                                                                           |
| `vendor/priv-app/CuttlefishService`, `system_ext/priv-app/CFSatelliteService`                                                     | cuttlefish-only apps                                                                                           |
| `vendor/overlay/cuttlefish_overlay_connectivity.apk`                                                                              | the image's connectivity RRO; the host network agent supplies capabilities                                     |
| `vendor/etc/init/motioncontext-service-default.rc`, `vendor/etc/vintf/manifest/motioncontext-service-default.xml`                 | a sample sensor-hub HAL; the session has no sensors                                                            |

## Removed for idle memory

The boot broadcasts start these apps, and they then stay resident. They serve
features that a desktop session doesn't have. Each one is blanked like the files
above. `tools/dev/footprint.sh` shows what a session holds.

| File                                                                                                          | Why                                                                                   |
| ------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------- |
| `system/priv-app/CellBroadcastLegacyApp`, `apex/com.android.cellbroadcast/priv-app/*`                         | emergency alerts arrive over a modem                                                  |
| `product/app/messaging`, `product/priv-app/ImsServiceEntitlement`                                             | SMS and carrier IMS need a modem                                                      |
| `product/app/Camera2`                                                                                         | no camera                                                                             |
| `system_ext/priv-app/Launcher3QuickStep` (windowed profiles only; phone mode keeps the image's)               | HOME is the system app’s empty activity; the desktop owns launches and task switching |
| `product/app/QuickSearchBox`                                                                                  | the home screen's search widget; nobody sees the launcher                             |
| `system/app/Traceur`, `system/priv-app/DeviceDiagnostics`, `system/priv-app/DynamicSystemInstallationService` | on-device tracing, trade-in diagnostics, GSI installs                                 |

APK paths inside APEXes carry the build id (`CellBroadcastApp@CP2A.260605.016`),
so check them when the pin moves. If one stops matching, the compat run's
idle-memory budget fails. The vendor APEX selection in
[image/bootconfig](../../image/bootconfig) drops sample HALs for hardware that
the session lacks. [init.innkeeper.rc](vendor/etc/init/hw/init.innkeeper.rc)
caps how many idle processes Android keeps.
