# Linux.apk

Run Android apps on desktop Linux as native-feeling apps. Each app gets its own
Wayland window and follows the desktop's dark mode, notifications, file pickers
and media controls. There's no VM, no visible Android device, and no root at
runtime.

How it works: a real AOSP (Android 17) userspace runs in a rootless, on-demand
session. Our code plugs in at Android's stable vendor interfaces (display, GPU,
audio, input), plus a desktop bridge for portals, notifications and MPRIS. See
[docs/STRATEGY.md](docs/STRATEGY.md).

## Status

**Pre-alpha. Nothing runs yet.**

| Milestone                                                       | Status                                                                                                                                          |
| --------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------- |
| M0 Session boots (prebuilt image, rootless namespace, binderfs) | 🚧 init, SurfaceFlinger (our HALs, ANGLE on RADV) and system_server run rootless; blocked on netd's eBPF/iptables (userspace shims in progress) |
| M1 Pixels in a Wayland window, first noarch apps                | ⏳                                                                                                                                              |
| M2 GPU, audio, per-app windows, launcher                        | ⏳                                                                                                                                              |
| M3 Desktop integration (portals, notifications, MPRIS, IME)     | ⏳                                                                                                                                              |
| M4 No-privilege mode (userspace binder, relocated image)        | ⏳                                                                                                                                              |
| M5 Packaging (per-app Flatpak/AppImage, RPM), aarch64 hosts     | ⏳                                                                                                                                              |
| M6 ARM translation on x86_64                                    | ⏳                                                                                                                                              |

Progress log: [docs/NOTES.md](docs/NOTES.md).

## Dev setup (Fedora)

Install [mise](https://mise.jdx.dev), then run `mise install`. Agent and
contributor context is in [AGENTS.md](AGENTS.md).

Fedora's SELinux policy stops ordinary processes from becoming a binder context
manager. Install the dev policy once:

```sh
cd selinux
checkmodule -M -m -o linuxapk_dev.mod linuxapk_dev.te
semodule_package -o linuxapk_dev.pp -m linuxapk_dev.mod
sudo semodule -i linuxapk_dev.pp
```

## License

AGPL-3.0-only.
