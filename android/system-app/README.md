# system-app

"Linux.apk System" (`dev.linuxapk.system`): a persistent, platform-signed
privileged app that is Android's side of the desktop.
`mise run build:system-app` (Gradle) installs `LinuxApkSystem.apk` to
`/system_ext/priv-app/LinuxApkSystem/` and its permissions XML to
`/system_ext/etc/permissions/`; system_server starts it at `systemReady` and
restarts it if it dies.

| Feature (`app/.../system/`)             | Bridge service | What it does                                                                            |
| --------------------------------------- | -------------- | --------------------------------------------------------------------------------------- |
| `wm/HomeActivity.kt`                    | none           | empty HOME on the windowless default display, replacing Launcher3/QuickStep             |
| `wm/`                                   | input          | one trusted virtual display and host window per task ([display](../../docs/display.md)) |
| `input/`, `ime/`                        | input          | host seat injection and the Linux.apk Keyboard ([input](../../docs/input.md))           |
| `integration/`                          | system         | desktop settings, notifications, clipboard, links, MPRIS, what keeps the session up     |
| `integration/{location,power,network}/` | hoststate      | the network location provider, battery saver, metering ([below](#host-state))           |
| `documents/`                            | documents      | the document picker and a DocumentsProvider for host files the user picked              |
| `apps/`                                 | apps           | desktop entries for apps installed inside Android; `IconDump` for the host              |
| `net/`                                  | none           | Flatpak mode: the host's network as Android's default network                           |
| `a11y/`                                 | a11y           | an accessibility service, on while the desktop's is, that sends app windows to AT-SPI   |

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

## Host state

`integration/HostStateFeature.kt` follows the bridge's hoststate service (the
battery goes to the Health HAL,
[android/vendor/health](../vendor/health/README.md)):

- **Location:** `HostLocationService` is Android's network location provider
  (`serviceVersion` 0, so microG's provider wins when the microG addon is
  installed), and a `gps` test provider gets the same fixes, since the session
  has no GNSS HAL; FusedLocation combines them. Android's requests start and
  stop one desktop session, at the most precise accuracy asked for. A refusal on
  the desktop turns both providers off for the session.
- **Power saver:** the desktop's power-saver profile is Android's battery saver
  (`setPowerSaveModeEnabled`), applied at each connect and change.
- **Metered:** applied only in Flatpak mode, to `net/HostNetworkFeature.kt`'s
  network agent.

Known limits: `gps` fixes carry `isMock`, which a few apps reject; a GNSS AIDL
HAL would remove that.

## Desktop handoff

The browser and `integration/handoff/` forward external VIEWs through OpenURI.
Custom Tabs VIEWs from apps with a verified App Link host use an installed
Android browser, allowing https sign-in redirects to return through Android App
Links. Ordinary links still use the desktop browser, including from those same
apps. The browser role stays with Linux.apk System.

A priority -1000 wildcard MIME filter handles files with no Android handler;
explicit scheme filters cover mailto, tel, sms, smsto, geo, market, magnet,
webcal(s) and ftp(s). Android handlers at normal priority take precedence.
Android 17 matches URI schemes literally: a wildcard scheme filter cannot catch
arbitrary untyped custom schemes.

The sharesheet's **Desktop** target offers **Open with a desktop app** (OpenFile
with `ask=true`), **Save to file…** (SaveFile), **Copy** (text, through
clipboard sync), and **Send by email** (Email, including attachments). Text
opens/saves as a `.txt` file. The bridge requests an xdg-activation token from
the source window's seat and parents portal dialogs with xdg-foreign.

Known limits:

- Without an Android browser installed, Custom Tabs still open on the desktop.
  An https App Link sign-in redirect then stays in the desktop browser and
  cannot return to the Android app; Linux.apk does not own desktop https links.
- Shares are capped at three attachments, 64 MiB each, and 32 KiB of text.
  Opened files and email attachments are retained in the instance cache until
  session shutdown, up to 256 MiB per session; saved copies are removed
  immediately. Shutdown cleans the cache; a later bridge reaps crash leftovers.
- Copy is offered only for text: Android content URIs cannot be pasted into a
  desktop file manager. For host → Android files, the static desktop entry
  offers Open and Share sections in its chooser, reusing read-only document
  grants. Share uses EXTRA_STREAM and ClipData, with SEND_MULTIPLE for batches.
  There is no standard host text-sharing hook in the supported desktop baseline
  (xdg-desktop-portal 1.20); text sharing from the host remains an upstream
  limit.
- Arbitrary untyped custom URI schemes need an explicit manifest filter; the
  Android 17 resolver has no catch-all scheme filter.

Tests: `:protocol` unit tests (`mise run test`) and the grouped e2e runner
(`mise run test:e2e -- input desktop a11y audio lifecycle`). These exercise app
windows, delivered input, desktop handoffs, accessible actions, audio at
PipeWire, and idle stop/alarm restart. The probes APK is never staged.
