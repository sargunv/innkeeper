# linuxapk-manager

The Linux.apk app (`dev.linuxapk.Manager`, GTK 4 and libadwaita): open or drop
an APK, see what it is and which permissions it wants, install it, then open or
remove it from a list. `bin/linuxapk-manager` plus its desktop data in `data/`
(entry, metainfo, icon and the `.apks`/`.xapk`/`.apkm` MIME types), built by
`mise run build:host`.

- **APK handler.** The shipped `.desktop` declares the APK and bundle types, so
  "Open With" offers the manager; `mise run install-dev` installs it for a
  checkout's stage. It is a single instance: opening another APK adds an install
  page to the running window.
- **First run** prepares the Android image ("Preparing Android"), and the first
  single-uid session shows a one-time banner that apps aren't isolated from each
  other (shared with the CLI's notice).
- **The list** comes from this profile's desktop entries, so it shows without
  booting Android, and a directory monitor follows CLI changes. At startup it
  repairs stale entries as `linuxapk list` does.
- Every core call runs on a worker thread (`worker.rs`); the UI thread never
  blocks on reading APKs, the session or the package manager.

Start reading at `src/window.rs` (list, session, drag and drop) and
`src/install.rs` (the confirmation page). Tests:
`cargo test -p linuxapk-manager` (error texts, permission names, progress
texts). For screenshots, use the nested desktop in
[docs/development.md](../../docs/development.md).

Selecting an installed app opens its per-app page (`src/app.rs`): storage use,
clear cache/data, runtime permission switches, Escape as Back, Run in
Background, force stop and uninstall. These controls use the bridge's versioned
session D-Bus API
([protocol](../../docs/bridge-protocol.md#desktop-session-api-version-1)). Clear
data and uninstall ask for confirmation. Operations disable the controls until
Android answers, and then refresh all values; errors offer a fresh read.

The main menu's Preferences page (`src/preferences.rs`, Ctrl+,) controls desktop
appearance, language/region and clock following (each on by default), idle
shutdown, app-menu mirroring and the optional installed microG addon. It also
keeps the single-uid notice visible and opens Android Settings in its own
window. Turning off desktop following keeps Android's current value; subsequent
changes in Android Settings are left alone. Turning off mirroring keeps existing
launchers; turning it back on catches up apps installed while it was off.

When idle, Android stops after 2 minutes by default (adjustable from 1 to 120).
Keep Running instead disables idle shutdown and requests login startup through
the Background portal; a denied request leaves the setting unchanged. Login
startup is only available for the main profile. microG uses the separately
installed addon described in [its README](../../android/microg/README.md); the
switch is unavailable without the addon and takes effect at the next Android
start. Play Services client libraries remain unsupported.

Preferences live in `$XDG_CONFIG_HOME/linuxapk[-<profile>]/preferences.toml`.
The page reads them without starting Android. A running session accepts updates
through `Session1`; otherwise they are saved for the next start. Desktop
following, mirroring and the idle timeout change live.

The install page renders adaptive icons on the host without starting Android:
static PNG/WebP layers, solid colours and vector paths (including groups,
transforms, strokes and alpha), resolved from the APK's binary resources. It
uses a circular mask and crops the layers' adaptive overscan. Theme tints,
colour state lists, gradients, clip paths, trimmed paths, animated drawables,
inset/layer-list drawables and resources supplied only by another split are not
previewed; those fall back to a legacy bitmap, then the generic icon. Android
renders the final installed icon.

The list shows a spinner and progress on the affected row while opening or
uninstalling, and an **Open** label for apps with windows. Session state follows
D-Bus name ownership and the bridge's `State` / `Changed` API, including windows
opened or closed outside the manager; there is no status polling timer.
