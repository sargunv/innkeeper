# linuxapk-core

The host library the CLI, the manager and the bridge share. No artifact of its
own; `cargo test -p linuxapk-core` tests it.

- **Operations** (`ops.rs`): inspect, install, uninstall, run and list, with
  progress through a callback so front ends choose how to show it. Install has
  two steps (`inspect`, then `install`), so the manager can confirm in between.
- **Install pipeline:** APKs and bundles (`.apks`, `.xapk`, `.apkm`;
  `bundle.rs`) are read in-process, without the Android SDK (`apk/`); ABI splits
  are filtered to the session's `ro.product.cpu.abilist`; one PackageInstaller
  session streams every APK through `install-write -` with no copy inside
  Android.
- **APK metadata** (`apk/`): the binary `AndroidManifest.xml` and the
  `resources.arsc` values it refers to, parsed per AOSP's `ResourceTypes.h`.
  Labels are picked per locale the way Android picks them (same language, same
  script, the closest region), so a desktop entry's `Name[xx]` matches what the
  app shows in that locale. Only what an install uses is decoded; anything
  malformed is an error, never a panic.
- **Icons** come from Android: the system app's `IconDump` runs under
  `app_process` and renders the launcher icon at 48 to 512 px, so adaptive and
  vector icons get the system mask. The APK's own PNGs are the fallback. Before
  installation, `apk/icon.rs` renders static adaptive/vector layers on the host
  with resvg; see the [manager limits](../manager/README.md).
- **Desktop entries** (`desktop.rs`, `xdg.rs`): `dev.linuxapk.<pkg>.desktop`
  with `StartupWMClass` matching the window's `app_id`, custom URL schemes as
  `x-scheme-handler/*` (never `http(s)`, `file`, `content` or `intent`), and
  `Exec` pointing at this installation's `linuxapk`. `xdg::refresh_entries`
  repairs entries whose program moved.
- **Inside the Flatpak** (`dynlauncher.rs`) those entries stay in the app's own
  data dir as its record, and the app menu gets each one through the
  DynamicLauncher portal, after the user confirms a dialog, as
  `dev.linuxapk.Manager.<pkg>.desktop` running `flatpak run --command=linuxapk`,
  which survives updates. Windows, notifications and MPRIS players use that id.
  Entry repair (`linuxapk list`, manager start) offers an entry again when it is
  missing or runs another command, unless the user declined it.
- **File metadata**: the system app queries PackageManager's exported, enabled,
  default VIEW filters that accept `content://` files and reports concrete MIME
  types through `IconDump`. Linux.apk's static Flatpak desktop entry registers
  common document, text and media types and forwards files through the document
  portal to an **Open with Android app…** chooser (see
  [the CLI](../cli/README.md)). Per-app host Open With entries remain deferred
  ([#71](https://github.com/sargunv/linux.apk/issues/71)).
- **Paths** (`paths.rs`): the prefix relative to the running executable and the
  XDG dirs; the session's own paths and the `LINUXAPK_PROFILE` parser come from
  the launcher's library ([host/launcher](../launcher/README.md)).
- **Session** (`launcher.rs`): starts the session on demand and waits for boot.
  Whether it runs (for the one-time single-uid notice,
  `ops::single_uid_notice`), its memory and whether the image is ready come from
  the launcher's library, in-process; start, stop and `exec` run
  `linuxapk-session`, whose `exec` must join the session's namespaces from a
  single-threaded process.
- **Errors** (`errors.rs`): `ErrorKind` context on anyhow errors lets front ends
  select user-facing copy without parsing diagnostics. PackageManager install
  codes are parsed once in core, including unknown future codes.
- **Untrusted input** (`validate.rs`): package names, labels, schemes, locales
  and URIs are validated before they reach a path, an entry or a command.

## Known limits

- The baseline DynamicLauncher portal rejects `[Desktop Action]` groups and has
  no silent update API; #68 is closed as an upstream limit, and the sandbox
  stays unchanged ([#68](https://github.com/sargunv/linux.apk/issues/68)).
