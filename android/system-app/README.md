# system-app

Innkeeper System (`dev.sargunv.innkeeper.system`) is a persistent,
platform-signed privileged app. It is Android's side of the desktop.
`mise run build:system-app` builds it with Gradle. It installs
`InnkeeperSystem.apk` to `/system_ext/priv-app/InnkeeperSystem/` and its
permissions XML to `/system_ext/etc/permissions/`. system_server starts the app
at `systemReady` and restarts it if it dies.

| Feature (`app/.../system/`)     | Bridge service | What it does                                                                                      |
| ------------------------------- | -------------- | ------------------------------------------------------------------------------------------------- |
| `wm/HomeActivity.kt`            | none           | an empty HOME on the windowless default display, in place of Launcher3/QuickStep                  |
| `wm/`                           | input          | a trusted virtual display and a host window for each task ([display](../../docs/display.md))      |
| `input/`, `ime/`                | input          | host seat injection and the Innkeeper Keyboard ([input](../../docs/input.md))                     |
| `integration/`                  | system         | desktop settings, notifications, clipboard, links, MPRIS, and what keeps the session up           |
| `integration/{location,power}/` | hoststate      | the network location provider and battery saver ([below](#host-state))                            |
| `integration/network/`          | hoststate      | the host's network as Android's default network, with its metering                                |
| `documents/`                    | documents      | the document picker, and a DocumentsProvider for host files that the user picked                  |
| `apps/`                         | apps           | desktop entries for apps installed inside Android                                                 |
| `a11y/`                         | a11y           | an accessibility service that sends app windows to AT-SPI while the desktop's accessibility is on |
| `alarms/`                       | none           | a tick every minute that delivers due inexact alarms, so ready jobs run on time                   |

Gradle modules:

- `:protocol` is pure JVM. It holds the bridge wire format (`Wire.kt`, checked
  against the Rust fixtures), input line parsing and translation, and the IME
  session. Its unit tests run in `mise run test`.
- `:inject` builds Android input events from `:protocol`'s events and injects
  them. The app and the probes share it.
- `:hidden-api` has compile-only stubs for hidden framework APIs
  (`IActivityTaskManager`, `IWindowManager`, and others). They resolve against
  the real framework at runtime.
- `:app` builds the APK. `bridge/BridgeConnection.kt` is the app's bridge
  client.
- `:framework-overlay` and `:settings-overlay` are resource-only,
  platform-signed RROs for desktop defaults. `init.innkeeper.rc` sets only the
  settings that each boot re-applies. The framework overlay enables per-display
  remote insets requests. It also fixes Android's stream volumes at maximum, so
  the desktop mixer is the only volume control. The SettingsProvider overlay
  turns off touch sounds and haptics on a fresh `/data`.
- `:probes` builds `InnkeeperProbes.apk`, with diagnostic activities and the
  input harness for the e2e and compat tests. It installs to `out/test/probes`
  and is not staged.

Notes:

- **Signing.** AGP leaves release APKs unsigned. `signRelease` signs them with
  AOSP's public test-keys platform key, which also signs the CI image. The build
  fetches the key from googlesource, checks its SHA-256 and caches it in
  `~/.cache/innkeeper-dev/keys`. The key is not committed.
- **Privileged permissions.** The image enforces
  `ro.control_privapp_permissions`. Every privileged permission that the
  manifest adds must also be listed in
  `etc/permissions/dev.sargunv.innkeeper.system.xml`, or system_server won't
  boot.
- `tools/gen-keymap.py` regenerates `EvdevKeymap.kt` from AOSP's `Generic.kl`.

## Host state

`integration/HostStateFeature.kt` follows the bridge's hoststate service. The
battery goes to the Health HAL instead
([android/vendor/health](../vendor/health/README.md)).

- **Location.** `HostLocationService` is Android's network location provider.
  Its `serviceVersion` is 0, so any other installed provider wins. The session
  has no GNSS HAL, so a `gps` test provider gets the same fixes. FusedLocation
  combines the two. Android's requests start and stop one desktop location
  session, at the most precise accuracy that any request asked for. If the
  desktop refuses, both providers turn off for the rest of the session.
- **Power saver.** The desktop's power-saver profile becomes Android's battery
  saver (`setPowerSaveModeEnabled`). It is applied at each connect and each
  change.
- **Metered.** The host connection's metering is applied to the network agent in
  `integration/network/HostNetworkFeature.kt`.

Known limits: `gps` fixes carry `isMock`, which a few apps reject. A GNSS AIDL
HAL would remove that flag.

## Desktop handoff

Innkeeper System holds the browser role. It and `integration/handoff/` forward
external VIEW intents to the desktop through OpenURI. A Custom Tabs VIEW from an
app with a verified App Link host goes to an installed Android browser instead.
That way an https sign-in redirect can return to the app as an App Link. The
same app's ordinary links still open on the desktop.

Files with no Android handler reach a `*/*` filter with priority -1000. Android
handlers at normal priority win. Android 17 matches URI schemes literally, so
the manifest lists each external scheme: mailto, tel, sms, smsto, geo, market,
magnet, webcal(s) and ftp(s). Any other custom scheme needs its own filter.

The sharesheet's **Desktop** target offers four actions:

- **Open with desktop app** uses OpenFile with `ask=true`.
- **Save to file…** uses SaveFile.
- **Copy** uses clipboard sync and handles text only.
- **Email** uses Email, with attachments.

Shared text becomes a `.txt` file. The bridge takes an xdg-activation token from
the source window's seat and parents portal dialogs with xdg-foreign.

From the host, the desktop entry's chooser offers Open and Share for files. Both
reuse read-only document grants. Share sends EXTRA_STREAM and ClipData, and uses
SEND_MULTIPLE for batches.

Known limits:

- Without an Android browser, Custom Tabs open on the desktop. An https App Link
  sign-in redirect then can't return to the app, because Innkeeper doesn't own
  desktop https links.
- Shares are capped at three attachments of 64 MiB each, and at 32 KiB of text.
  Opened files and email attachments stay in the profile cache until the session
  shuts down, up to 256 MiB per session. Saved copies are removed at once. The
  next bridge reaps files that a crash leaves behind.
- Copy handles text only, because a desktop file manager can't paste Android
  content URIs.
- xdg-desktop-portal 1.20 has no standard hook for sharing text from the host.

Tests: `:protocol` unit tests (`mise run test`) and
`mise run test:e2e -- input desktop a11y audio lifecycle`.
