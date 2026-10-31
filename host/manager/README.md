# innkeeper-manager

The Innkeeper app (`dev.sargunv.Innkeeper`, GTK 4 and libadwaita): open or drop
an APK, see what it is and which permissions it wants, install it, then open,
configure or remove it from a list. `bin/innkeeper-manager` plus its desktop
data in `data/` (entry, metainfo, icon and the `.apks`/`.xapk`/`.apkm` MIME
types), built by `mise run build:host`.
`data/icons/dev.sargunv.Innkeeper.source.svg` is the editable App Icon Preview
source of the shipped icon.

Start reading at `src/window.rs` (the list, the session row, drag and drop).
Every core call runs on a worker thread (`src/worker.rs`), so the UI thread
never blocks on reading APKs, the session or the package manager. Tests:
`cargo test -p innkeeper-manager` (error, permission and progress texts). For
screenshots, use the nested desktop in
[docs/development.md](../../docs/development.md).

## Main window

- **APK handler:** the desktop entry runs `innkeeper-manager %F` and declares
  only the APK and bundle types, so "Open With" offers the manager for those;
  `mise run package -- --install` installs it for a checkout's stage. It is
  single-instance: opening another APK adds an install page to the running
  window.
- **First run** prepares the Android image ("Preparing Android").
- **The list** comes from the profile's desktop entries, so it shows without
  booting Android, and a directory monitor follows CLI changes. At startup it
  repairs stale entries as `innkeeper list` does. A row shows a spinner while
  its app opens or uninstalls, and **Open** while it has windows.
- **Session state** follows D-Bus name ownership and the bridge's `State` /
  `Changed` API, including windows opened or closed elsewhere; nothing polls.

## Install page

`src/install.rs`. Adaptive icons are rendered on the host without starting
Android: static PNG/WebP layers, solid colours and vector paths (groups,
transforms, strokes, alpha), resolved from the APK's binary resources, with a
circular mask and the adaptive overscan cropped. Theme tints, colour state
lists, gradients, clip paths, trimmed paths, animated drawables, inset and
layer-list drawables, and resources only another split supplies fall back to a
bitmap icon, then the generic one. Android renders the installed icon.

## App page

`src/app.rs`: storage use, clear cache or data, runtime permission switches,
Escape as Back, Run in Background, force stop and uninstall, through the
bridge's session D-Bus API
([protocol](../../docs/bridge-protocol.md#desktop-session-api-version-1)).
Clearing data and uninstalling ask first. Controls stay disabled until Android
answers, then every value is read again; an error offers a fresh read.

## Profile Settings

`src/preferences.rs` (Ctrl+,): the shown profile's settings, read without
starting Android: its Display, windowed or a phone preset for its next start
([phone mode](../../docs/display.md#phone-mode)); following the desktop's
appearance, language/region and clock (each on by default); its idle timeout;
app-menu mirroring; and an adb switch that applies at once
([development.md](../../docs/development.md#adb-into-a-profile)). It also says
that apps aren't isolated from each other and opens Android Settings in its own
window.

- Settings live in the profile's one settings file
  ([session.md](../../docs/session.md#profiles)). A running profile takes
  changes through `Session1`'s `ReloadSettings`: desktop following, mirroring
  and the idle timeout change live, the display at the next start.
- Turning off desktop following keeps Android's current value and leaves later
  changes in Android Settings alone. Turning off mirroring keeps existing
  launchers; turning it back on adds apps installed meanwhile.
- When idle, Android stops after 2 minutes by default (1 to 120). **Keep
  Running** is every profile's: no profile stops when idle, and the Background
  portal's one login command starts them all. If the portal denies it, the
  setting stays off.

## Profiles

`src/profiles.rs`: every profile and whether it runs, to create, rename, delete,
start and stop. The window shows one profile's apps, named under its title;
**Show Apps** switches to another, which the manager shows again the next time
it opens (`$XDG_STATE_HOME/innkeeper/manager-profile`; `--profile NAME`
overrides it). Opening the manager with no profile creates `default`. Deleting
asks first and removes the profile's apps, data, settings and app-menu entries.
