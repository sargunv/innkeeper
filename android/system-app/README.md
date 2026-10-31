# system-app

"Innkeeper System" (`dev.sargunv.innkeeper.system`): a persistent,
platform-signed privileged app that is Android's side of the desktop.
`mise run build:system-app` (Gradle) installs `InnkeeperSystem.apk` to
`/system_ext/priv-app/InnkeeperSystem/` and its permissions XML to
`/system_ext/etc/permissions/`; system_server starts it at `systemReady` and
restarts it if it dies.

| Feature (`app/.../system/`)             | Bridge service | What it does                                                                            |
| --------------------------------------- | -------------- | --------------------------------------------------------------------------------------- |
| `wm/HomeActivity.kt`                    | none           | empty HOME on the windowless default display, replacing Launcher3/QuickStep             |
| `wm/`                                   | input          | one trusted virtual display and host window per task ([display](../../docs/display.md)) |
| `input/`, `ime/`                        | input          | host seat injection and the Innkeeper Keyboard ([input](../../docs/input.md))           |
| `integration/`                          | system         | desktop settings, notifications, clipboard, links, MPRIS, what keeps the session up     |
| `integration/{location,power,network}/` | hoststate      | the network location provider, battery saver, metering ([below](#host-state))           |
| `documents/`                            | documents      | the document picker and a DocumentsProvider for host files the user picked              |
| `apps/`                                 | apps           | desktop entries for apps installed inside Android; `IconDump` for the host              |
| `net/`                                  | none           | Flatpak mode: the host's network as Android's default network                           |
| `a11y/`                                 | a11y           | an accessibility service, on while the desktop's is, that sends app windows to AT-SPI   |
| `alarms/`                               | none           | a minute tick that delivers due inexact alarms, so ready jobs run on time               |

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
- `:wm-overlay`: resource-only, platform-signed RRO enabling per-display remote
  insets requests, and fixing Android's stream volumes at maximum so the desktop
  mixer is the only volume control.
- `:probes`: `InnkeeperProbes.apk`, diagnostic activities and the input harness
  for the e2e and compat tests. Installed to `out/test/probes`, never staged.

Notes:

- **Signing:** AGP leaves release APKs unsigned; `signRelease` signs them with
  AOSP's public test-keys platform key, which also signs the CI image. The key
  is fetched from googlesource, checked by SHA-256 and cached in
  `~/.cache/innkeeper-dev/keys`, never committed.
- **Privileged permissions:** the image enforces
  `ro.control_privapp_permissions`, so every privileged permission the manifest
  adds must also be listed in
  `etc/permissions/dev.sargunv.innkeeper.system.xml`, or system_server won't
  boot.
- **IconDump** (`apps/IconDump.kt`) runs under `app_process` from the installed
  APK for `innkeeper install`, so the host needs no Android SDK to render icons.
- `tools/gen-keymap.py` regenerates `EvdevKeymap.kt` from AOSP's `Generic.kl`.

## Host state

`integration/HostStateFeature.kt` follows the bridge's hoststate service (the
battery goes to the Health HAL,
[android/vendor/health](../vendor/health/README.md)):

- **Location:** `HostLocationService` is Android's network location provider
  (`serviceVersion` 0, so any other installed provider wins), and a `gps` test
  provider gets the same fixes, since the session has no GNSS HAL; FusedLocation
  combines them. Android's requests start and stop one desktop session, at the
  most precise accuracy asked for. A refusal on the desktop turns both providers
  off for the session.
- **Power saver:** the desktop's power-saver profile is Android's battery saver
  (`setPowerSaveModeEnabled`), applied at each connect and change.
- **Metered:** applied only in Flatpak mode, to `net/HostNetworkFeature.kt`'s
  network agent.

Known limits: `gps` fixes carry `isMock`, which a few apps reject; a GNSS AIDL
HAL would remove that.

## Desktop handoff

Innkeeper System holds the browser role, and it and `integration/handoff/`
forward external VIEWs to the desktop through OpenURI. A Custom Tabs VIEW from
an app with a verified App Link host goes to an installed Android browser
instead, so an https sign-in redirect can return to the app as an App Link; the
same app's ordinary links still open on the desktop.

Files with no Android handler reach a priority -1000 `*/*` filter; Android
handlers at normal priority win. Android 17 matches URI schemes literally, so
the manifest lists each external scheme (mailto, tel, sms, smsto, geo, market,
magnet, webcal(s), ftp(s)), and any other custom scheme needs its own filter.

The sharesheet's **Desktop** target offers **Open with a desktop app** (OpenFile
with `ask=true`), **Save to file…** (SaveFile), **Copy** (text only, through
clipboard sync) and **Send by email** (Email, with attachments). Text is shared
as a `.txt` file. The bridge takes an xdg-activation token from the source
window's seat and parents portal dialogs with xdg-foreign.

From the host, the desktop entry's chooser offers Open and Share for files,
reusing read-only document grants; Share sends EXTRA_STREAM and ClipData, with
SEND_MULTIPLE for batches.

Known limits:

- Without an Android browser, Custom Tabs open on the desktop, and an https App
  Link sign-in redirect can't return to the app: Innkeeper doesn't own desktop
  https links.
- Shares are capped at three attachments, 64 MiB each, and 32 KiB of text.
  Opened files and email attachments stay in the profile cache until session
  shutdown, up to 256 MiB per session; saved copies are removed at once. A later
  bridge reaps what a crash leaves.
- Copy is text only: a desktop file manager can't paste Android content URIs.
- Sharing text from the host has no standard hook in xdg-desktop-portal 1.20.

Tests: `:protocol` unit tests (`mise run test`) and
`mise run test:e2e -- input desktop a11y audio lifecycle`.
