# android

Everything that runs inside the Android session. Each module installs a tree
that mirrors Android's partition paths under the stage's
`share/innkeeper/android/`, which the session stacks over the image's partitions
([docs/session.md](../docs/session.md#overlay-stack)). Nothing here patches the
image.

| Module                                       | Installs to                                                                          | Loaded by                                                        |
| -------------------------------------------- | ------------------------------------------------------------------------------------ | ---------------------------------------------------------------- |
| [vendor/graphics](vendor/graphics/README.md) | `/vendor/bin/hw`, `/vendor/lib64/hw/mapper.innkeeper.so`, `/vendor/etc/{init,vintf}` | init (allocator, composer); every process using gralloc (mapper) |
| [vendor/health](vendor/health/README.md)     | `/vendor/bin/hw`, `/vendor/etc/{init,vintf}`                                         | init (`IHealth/default`, the host's battery)                     |
| [vendor/vulkan](vendor/vulkan/README.md)     | `/vendor/lib64/hw/vulkan.{radv,lvp}.so`, `/vendor/etc/permissions`                   | libvulkan in apps, SurfaceFlinger and ANGLE                      |
| [vendor/audio](vendor/audio/README.md)       | `/apex/com.android.hardware.audio/lib64/libtinyalsav2.so`, `/vendor/etc`             | the AIDL audio HAL                                               |
| [system](system/README.md)                   | `/system/bin/{innkeeper-exec,iptables,…}`, rc                                        | init (exec relay), netd (no-op firewall)                         |
| [system-app](system-app/README.md)           | `/system_ext/priv-app/InnkeeperSystem`, `/system_ext/etc/permissions`                | system_server, as a persistent privileged app                    |
| [overlay](overlay/README.md)                 | rc, XML and blank files in every partition                                           | init, system_server, PackageManager                              |
| [microg](microg/README.md)                   | the addons layer, `$XDG_DATA_HOME/innkeeper/addons/microg`                           | PackageManager (optional, `mise run microg`)                     |

Props for `/vendor/build.prop` go to the stage's `share/innkeeper/props.d/`; the
launcher merges them at start.

- **NDK only.** C and C++ build in the root CMake superbuild with the NDK pin
  (`tools/build/toolchain.env`). Vendor code links only LL-NDK libraries plus
  what we install in `/vendor/lib64`; libc++ is static.
- **AOSP interfaces.** `vendor/third_party/aosp` holds every frozen AIDL
  snapshot the HALs implement and the AOSP headers they build against, copied
  from one AOSP tag by `vendor/tools/sync-aosp.sh`. At configure time
  `vendor/tools/gen-aidl.sh` generates each HAL's NDK backend with the
  build-tools' `aidl`.
- **Gradle** builds the system app with the SDK's `platforms;android-37.0` and
  build-tools; the wrapper is checked in.
