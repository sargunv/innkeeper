# Innkeeper

Run Android and OpenHarmony apps on Linux. Each Android app opens in its own
Wayland window and uses the desktop's dark mode, notifications, file pickers,
clipboard and media controls. Innkeeper uses no virtual machine and no root
access, and it shows no Android device screen.

Innkeeper runs an Android 17 userspace inside its Flatpak sandbox. The session
starts when an app needs it. The Android image is Google's AOSP build from
Android's public CI. Innkeeper downloads it and checks its checksum on your
machine. Innkeeper implements Android's stable vendor interfaces for display,
GPU, audio and input, and connects them to your desktop.

| Wikipedia                                                                                                                     | F-Droid                                                                                                                  |
| ----------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------ |
| ![Wikipedia reading an article in Innkeeper](https://github.com/user-attachments/assets/68722463-49e5-49fd-80d8-a7b2c498d612) | ![F-Droid app store in Innkeeper](https://github.com/user-attachments/assets/19d9cd5c-673b-4584-9955-f9d169c54822)       |
| **Mindustry**                                                                                                                 | **Shattered Pixel Dungeon**                                                                                              |
| ![Mindustry in Innkeeper](https://github.com/user-attachments/assets/c52f2c4b-0a3f-45bb-a521-0fa0300f545a)                    | ![Shattered Pixel Dungeon in Innkeeper](https://github.com/user-attachments/assets/2f450098-1226-45e9-91e2-407423303333) |
| **Tomato** (Jetpack Compose)                                                                                                  | **OpenCalc**                                                                                                             |
| ![Tomato (Jetpack Compose) in Innkeeper](https://github.com/user-attachments/assets/3229708b-5d53-4cc7-a8df-d096d7d5812f)     | ![OpenCalc in Innkeeper](https://github.com/user-attachments/assets/4ff9dca7-6851-4285-93ca-1e3c2250c66d)                |

## Install

You need an x86_64 Linux desktop on Wayland with a GPU, Linux 6.5 or newer, and
Flatpak 1.15.6 or newer with the Flathub remote. Download
`dev.sargunv.Innkeeper-snapshot.flatpak` from the
[snapshot release](https://github.com/sargunv/innkeeper/releases/tag/snapshot)
and install it:

```sh
flatpak install --user ./dev.sargunv.Innkeeper-snapshot.flatpak
```

Innkeeper doesn't update itself yet. To update, install a newer file with
`--reinstall`.

## Use it

To install an app, open its APK with **Innkeeper**. Innkeeper shows the app's
details and the permissions it requests, installs it, and adds it to your app
menu. Each app opens in its own window. The first use downloads the Android
image, which is about 1.2 GB.

You can also use a terminal, for example
`flatpak run dev.sargunv.Innkeeper install App.apk`. The `--help` option lists
every command.

Android starts when an app needs it. It stops a couple of minutes after the last
app window closes.

Each profile has its own apps and data. A profile can also show a whole phone or
tablet screen in one window. Innkeeper's Profiles page and each profile's
settings control these options.

A profile can run OpenHarmony instead of Android. You choose the system when you
create the profile, for example with
`innkeeper profile create --system ohos harmony`. OpenHarmony support is
experimental. Its built-in apps show in one phone window, and it doesn't support
desktop input or app installation yet ([ohos](ohos/README.md)).

All apps run as one uid, so they are not isolated from each other. Native
desktop apps aren't isolated from each other either
([security](docs/security.md#app-isolation)).

An Innkeeper upgrade can include a newer Android. The newer Android migrates the
profile's data to a format that older versions can't read. Before you upgrade,
stop the profile and copy its whole data directory somewhere safe with `cp -a`.
For the default profile, the directory is
`~/.var/app/dev.sargunv.Innkeeper/data/innkeeper/data-default`. To go back,
restore that copy and use the older Innkeeper.

## For Android developers

Turn on adb in a profile's settings in Innkeeper. The profile then listens for
adb on a loopback port, as the Android Emulator does. `adb devices`, Android
Studio and Gradle find it without `adb connect` and list it with an
`emulator-<n>` serial. Use that serial, because Gradle's `connectedAndroidTest`
fails with `host:port` serials. Innkeeper authorizes only the key in your
`~/.android/adbkey.pub`.

Run instrumented tests that take screenshots in a phone or tablet profile. In
windowed mode, each app window is a separate display, so a capture of the
default display doesn't show the app.

## How is this different from Waydroid?

Waydroid runs a custom LineageOS image in an LXC container that needs root. It
uses the kernel's binder driver and gives each app a separate uid. Innkeeper
needs no root, kernel module or custom image. It runs Google's unmodified AOSP
build inside its Flatpak sandbox and emulates binder in userspace. As a result,
Android apps are not isolated from each other. Each app gets its own desktop
window, which uses the desktop's portals, notifications and media controls.

## Status

Innkeeper is pre-alpha. A warm Android boot takes about 3 s. Apps built with
Compose, Flutter, React Native, Capacitor and Godot work, each in its own window
with sound. Innkeeper doesn't support these apps:

- Apps that need Play Integrity, such as banking apps.
- VPN apps, because the sandbox has no TUN device.
- Per-app firewalls and per-app data usage.
- ARM-only apps, which include most games. ARM translation is planned
  ([milestones](https://github.com/sargunv/innkeeper/milestones)).

## Develop

Innkeeper builds with [mise](https://mise.jdx.dev), the Android NDK, podman and
clang:

```sh
mise install       # toolchains and the pre-commit hook
mise run build     # everything, staged into out/stage
mise run test      # unit tests
```

Read [docs/architecture.md](docs/architecture.md) first. It describes what each
directory contains. [docs/development.md](docs/development.md) covers
prerequisites, sessions and tests. [AGENTS.md](AGENTS.md) has the rules for
contributors and agents.

## License

AGPL-3.0-or-later.
