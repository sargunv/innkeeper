# linuxapk-core

The host library the CLI, the manager and the bridge share. No artifact of its
own; `cargo test -p linuxapk-core` tests it.

- **Operations** (`ops.rs`): inspect, install, uninstall, run and list, with
  progress through a callback so front ends choose how to show it. Install has
  two steps (`inspect`, then `install`), so the manager can confirm in between.
- **Install pipeline:** APKs and bundles (`.apks`, `.xapk`, `.apkm`;
  `bundle.rs`) are read with `aapt2` (`manifest.rs`); ABI splits are filtered to
  the session's `ro.product.cpu.abilist`; one PackageInstaller session streams
  every APK through `install-write -` with no copy inside Android.
- **Icons** come from Android: the system app's `IconDump` runs under
  `app_process` and renders the launcher icon at 48 to 512 px, so adaptive and
  vector icons get the system mask. The APK's own PNGs are the fallback.
- **Desktop entries** (`desktop.rs`, `xdg.rs`): `dev.linuxapk.<pkg>.desktop`
  with `StartupWMClass` matching the window's `app_id`, custom URL schemes as
  `x-scheme-handler/*` (never `http(s)`, `file`, `content` or `intent`), and
  `Exec` pointing at this installation's `linuxapk`. `xdg::refresh_entries`
  repairs entries whose program moved.
- **Paths** (`paths.rs`): the prefix relative to the running executable, the XDG
  dirs, and the only `LINUXAPK_INSTANCE` parser.
- **Session** (`launcher.rs`): starts the session on demand, waits for boot, and
  reads `uid-mode=` from `linuxapk-session status` for the one-time single-uid
  notice (`ops::single_uid_notice`).
- **Untrusted input** (`validate.rs`): package names, labels, schemes, locales
  and URIs are validated before they reach a path, an entry or a command.
