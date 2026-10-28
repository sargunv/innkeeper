# android

Everything that runs inside the Android session. Each module installs a tree
that mirrors Android's partition paths under the stage's
`share/linuxapk/android/`, which the session stacks over the image's partitions
([docs/session.md](../docs/session.md#overlay-stack)). Nothing here patches the
image.

| Module                                                 | Installs to                                                                         | Loaded by                                                        |
| ------------------------------------------------------ | ----------------------------------------------------------------------------------- | ---------------------------------------------------------------- |
| [vendor/graphics](vendor/graphics/README.md)           | `/vendor/bin/hw`, `/vendor/lib64/hw/mapper.linuxapk.so`, `/vendor/etc/{init,vintf}` | init (allocator, composer); every process using gralloc (mapper) |
| [vendor/health](vendor/health/README.md)               | `/vendor/bin/hw`, `/vendor/etc/{init,vintf}`                                        | init (`IHealth/default`, the host's battery)                     |
| [vendor/vulkan](vendor/vulkan/README.md)               | `/vendor/lib64/hw/vulkan.{radv,lvp}.so`, `/vendor/etc/permissions`                  | libvulkan in apps, SurfaceFlinger and ANGLE                      |
| [vendor/audio](vendor/audio/README.md)                 | `/apex/com.android.hardware.audio/lib64/libtinyalsav2.so`, `/vendor/etc`            | the AIDL audio HAL                                               |
| [vendor/bridge-client](vendor/bridge-client/README.md) | nothing (static library)                                                            | linked into the composer, the Health HAL and `libtinyalsav2`     |
| [system](system/README.md)                             | `/system/bin/{linuxapk-exec,xtables-nft-multi,linuxapk-iptables}`, rc               | init (exec relay); netd (iptables)                               |
| [system-app](system-app/README.md)                     | `/system_ext/priv-app/LinuxApkSystem`, `/system_ext/etc/permissions`                | system_server, as a persistent privileged app                    |
| [overlay](overlay/README.md)                           | rc, XML and blank files in every partition                                          | init, system_server, PackageManager                              |
| [microg](microg/README.md)                             | the addons layer, `$XDG_DATA_HOME/linuxapk/addons/microg`                           | PackageManager (optional, `mise run microg`)                     |

Props for `/vendor/build.prop` go to the stage's `share/linuxapk/props.d/`; the
launcher merges them at start.

- **NDK only.** C and C++ build in the root CMake superbuild with the NDK pin
  (`tools/build/toolchain.env`). Vendor code links only LL-NDK libraries plus
  what we install in `/vendor/lib64`; libc++ is static.
- **Gradle** builds the system app with the SDK's `platforms;android-37.0` and
  build-tools; the wrapper is checked in.
