# Linux.apk

Run Android apps on desktop Linux as native-feeling apps. Each app gets its own
Wayland window and follows the desktop's dark mode, notifications, file pickers
and media controls. There's no VM, no visible Android device, and no root at
runtime.

How it works: a real AOSP (Android 17) userspace runs in a rootless, on-demand
session. The image is Google's own AOSP build from Android's public CI,
downloaded and checksummed at setup ([docs/image.md](docs/image.md)). Our code
plugs in at Android's stable vendor interfaces (display, GPU, audio, input),
plus a desktop bridge for portals, notifications and MPRIS. See
[docs/STRATEGY.md](docs/STRATEGY.md).

## Status

**Pre-alpha.** Android 17 boots rootless and 30 of 31 test apps work
([COMPATIBILITY.md](COMPATIBILITY.md)), across Compose, Flutter, React Native,
Capacitor and Godot apps, including StreetComplete's MapLibre map and video
playback. Each app opens in its own window with sound. Next: desktop integration
(M3) and no-privilege mode (M4).

| Milestone                                                       | Status                                                                                                                                                                                                                                                                              |
| --------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| M0 Session boots (prebuilt image, rootless namespace, binderfs) | ✅ `sys.boot_completed` rootless in ~16 s cold / ~4 s warm; no AOSP patches; netd via userspace eBPF emulation + nftables iptables                                                                                                                                                  |
| M1 Pixels in a Wayland window, first noarch apps                | ✅ display in a KDE Wayland window (dmabuf), mouse/keyboard/touch input, networking, media decode; 15/16 corpus apps work, including Compose apps and x86_64 games                                                                                                                  |
| M2 GPU, audio, per-app windows, launcher                        | ✅ RADV/ANGLE GPU; one window per app (title bars on KDE, client-side on GNOME); PipeWire sound (≈90 ms); `linuxapk` CLI and manager app; 15/16 corpus in per-app mode                                                                                                              |
| M3 Desktop integration (portals, notifications, MPRIS, IME)     | 🚧 dark mode + accent + locale/time zone sync, notifications (actions, reply), clipboard, links/OAuth in host browser, MPRIS, IME (`text-input-v3`), live scale changes ✅; file picker and app-menu mirroring landing; microG partial ([docs/integration.md](docs/integration.md)) |
| M4 No-privilege mode (userspace binder, AOSP CI image)          | 🚧 AOSP CI image ✅ (default; no local AOSP build or redistribution needed); userspace binder in progress                                                                                                                                                                           |
| M5 Packaging (per-app Flatpak/AppImage, RPM), aarch64 hosts     | ⏳                                                                                                                                                                                                                                                                                  |
| M6 ARM translation on x86_64                                    | ⏳                                                                                                                                                                                                                                                                                  |

Progress log: [docs/NOTES.md](docs/NOTES.md).

Real apps running in Linux.apk on KDE Plasma (Wayland):

| Wikipedia                                                                                                                     | F-Droid                                                                                                                  |
| ----------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------ |
| ![Wikipedia reading an article in Linux.apk](https://github.com/user-attachments/assets/68722463-49e5-49fd-80d8-a7b2c498d612) | ![F-Droid app store in Linux.apk](https://github.com/user-attachments/assets/19d9cd5c-673b-4584-9955-f9d169c54822)       |
| **Mindustry**                                                                                                                 | **Shattered Pixel Dungeon**                                                                                              |
| ![Mindustry in Linux.apk](https://github.com/user-attachments/assets/c52f2c4b-0a3f-45bb-a521-0fa0300f545a)                    | ![Shattered Pixel Dungeon in Linux.apk](https://github.com/user-attachments/assets/2f450098-1226-45e9-91e2-407423303333) |
| **Tomato** (Jetpack Compose)                                                                                                  | **OpenCalc**                                                                                                             |
| ![Tomato (Jetpack Compose) in Linux.apk](https://github.com/user-attachments/assets/3229708b-5d53-4cc7-a8df-d096d7d5812f)     | ![OpenCalc in Linux.apk](https://github.com/user-attachments/assets/4ff9dca7-6851-4285-93ca-1e3c2250c66d)                |

## Try it

Open an APK with the **Linux.apk** manager app (GTK4/libadwaita): it shows what
the app is and which permissions it wants, installs it, and adds it to your app
menu. Each app then opens in its own window. The same is scriptable with the
`linuxapk` CLI: `linuxapk install some.apk`, and `linuxapk setup` makes APKs
open in the manager. See [docs/gui.md](docs/gui.md) and
[docs/cli.md](docs/cli.md).

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
