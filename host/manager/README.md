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
- **The list** comes from this instance's desktop entries, so it shows without
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
