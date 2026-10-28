# system-app

"Linux.apk System" (`dev.linuxapk.system`): a persistent, platform-signed
privileged app that is Android's side of the desktop.
`mise run build:system-app` (Gradle) installs `LinuxApkSystem.apk` to
`/system_ext/priv-app/LinuxApkSystem/` and its permissions XML to
`/system_ext/etc/permissions/`; system_server starts it at `systemReady` and
restarts it if it dies.

| Feature (`app/.../system/`) | Bridge service | What it does                                                                           |
| --------------------------- | -------------- | -------------------------------------------------------------------------------------- |
| `wm/`                       | input          | one trusted virtual display and host window per app ([display](../../docs/display.md)) |
| `input/`, `ime/`            | input          | host seat injection and the Linux.apk Keyboard ([input](../../docs/input.md))          |
| `integration/`              | system         | desktop settings, notifications, clipboard, links, MPRIS                               |
| `documents/`                | documents      | the document picker and a DocumentsProvider for host files the user picked             |
| `apps/`                     | apps           | desktop entries for apps installed inside Android; `IconDump` for the host             |

Gradle modules:

- `:protocol`: pure JVM. The bridge wire format (`Wire.kt`, checked against the
  Rust fixtures), input line parsing and translation, the IME session. Its unit
  tests run in `mise run test`.
- `:inject`: Android input events from `:protocol`'s, and their injection;
  shared by the app and the probes.
- `:hidden-api`: compile-only stubs for hidden framework APIs
  (`IActivityTaskManager`, `IWindowManager`, …), resolved against the real
  framework at runtime.
- `:app`: the APK. `bridge/BridgeConnection.kt` is the one Kotlin client of the
  bridge.
- `:probes`: `LinuxApkProbes.apk`, diagnostic activities and the input harness
  for the e2e and compat tests. Installed to `out/test/probes`, never staged.

Notes:

- **Signing:** AGP leaves release APKs unsigned; `signRelease` signs them with
  AOSP's public test-keys platform key, which also signs the CI image. The key
  is fetched from googlesource, checked by SHA-256 and cached in
  `~/.cache/linuxapk-dev/keys`, never committed.
- **Privileged permissions:** the image enforces
  `ro.control_privapp_permissions`, so every privileged permission the manifest
  adds must also be listed in `etc/permissions/dev.linuxapk.system.xml`, or
  system_server won't boot.
- **IconDump** (`apps/IconDump.kt`) runs under `app_process` from the installed
  APK for `linuxapk install`, so the host needs no Android SDK to render icons.
- `tools/gen-keymap.py` regenerates `EvdevKeymap.kt` from AOSP's `Generic.kl`.

Tests: `:protocol` unit tests (`mise run test`); `tests/e2e/system-app`
(`mise run test:e2e`) checks the app's permissions, its bridge connections,
windows through `dev.linuxapk.LAUNCH` and input injection with the probes.
