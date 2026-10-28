# overlay

The checked-in config layer over the image's partitions, one dir per partition.
`mise run build:data` copies it into the stage's `share/linuxapk/android/`,
which the session stacks above the image
([docs/session.md](../../docs/session.md#overlay-stack)). A zero-byte file or an
empty manifest blanks the image's file of the same path.

## Forked files

Copies of image files with our edits (each edit is marked `linuxapk:`). They
must be diffed against the image's copies whenever the pin moves
([image/README.md](../../image/README.md#bumping-the-pin)).

| File                                                | Why                                                                                                                                            |
| --------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| `system/etc/init/hw/init.rc`                        | no ueventd (`ro.cold_boot_done` set directly), `sys.use_memfd` (no ashmem), the flattened `/apex` bind instead of apexd-bootstrap, no binderfs |
| `system/etc/init/hw/init.zygote64.rc`               | preloads the shim (`selinux_android_setcon`)                                                                                                   |
| `system/etc/init/{servicemanager,vold,installd}.rc` | preload the shim (SELinux status and labels); installd also sets `LINUXAPK_VSTAT`                                                              |
| `system/etc/init/apexd.rc`                          | runs `linuxapk-apexd`, which shows apexd the pre-mounted APEXes                                                                                |
| `system/etc/init/netd.rc`                           | `netd1shot` is a no-op; netd runs once instead of crash-looping                                                                                |
| `system/etc/cgroups.json`                           | cgroup2 only: the session has no v1 hierarchies                                                                                                |
| `apex/com.android.tethering/etc/netbpfload.35rc`    | runs netbpfload through `linuxapk-bpfload` against the emulated `bpf()`                                                                        |

## Added files

| File                                                       | Why                                                                              |
| ---------------------------------------------------------- | -------------------------------------------------------------------------------- |
| `vendor/etc/init/hw/init.linuxapk.rc`                      | the device rc for `androidboot.hardware=linuxapk`, in place of cuttlefish's      |
| `system/bin/linuxapk-apexd`, `linuxapk-bpfload`            | wrappers with private mount namespaces (see their headers)                       |
| `system/bin/linuxapk-first-boot`                           | desktop defaults once per `/data`: no touch sounds or haptics                    |
| `product/overlay/config/config.xml`                        | gesture navigation by default: three-button mode crash-loops Launcher3's taskbar |
| `vendor/etc/permissions/linuxapk-unavailable-hardware.xml` | no Bluetooth: without its HAL, `com.android.bluetooth` aborts in a loop          |

## Blanked files

| File                                                                                                                              | What it was                                                                |
| --------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------- |
| `vendor/etc/init/init.cutf_cvm.rc`                                                                                                | cuttlefish's device rc: VM kernel modules, fstab, adb over vsock           |
| `vendor/etc/init/seriallogging.rc`                                                                                                | `logcat -b all` to a virtio console                                        |
| `system_ext/etc/init/rpmb_dev.test.system.rc`                                                                                     | an RPMB mock for Trusty test VMs                                           |
| `product/etc/init/init.rc`                                                                                                        | `tombstone_transmit` over vsock                                            |
| `vendor/etc/init/android.hardware.sensors-service-multihal.rc`, `vendor/etc/vintf/manifest/android.hardware.sensors-multihal.xml` | the sensors HAL; the session has no sensors                                |
| `vendor/etc/vintf/manifest/{face,fingerprint}-default.xml`                                                                        | biometrics HALs declared but never started                                 |
| `vendor/etc/vintf/manifest/mapper.minigbm.xml`                                                                                    | the minigbm mapper; ours replaces it                                       |
| `vendor/priv-app/CuttlefishService`, `system_ext/priv-app/CFSatelliteService`                                                     | cuttlefish-only apps                                                       |
| `vendor/overlay/cuttlefish_overlay_connectivity.apk`                                                                              | an RRO that made pasta's `eth1` a restricted network, so apps were offline |
