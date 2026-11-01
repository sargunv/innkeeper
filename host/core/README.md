# innkeeper-core

`innkeeper-core` is the library that the CLI, the manager and the bridge share.
It builds no artifact of its own. `cargo test -p innkeeper-core` runs its tests.

- **Operations** (`ops.rs`): inspect, install, uninstall, run and list. They
  report progress through a callback, so each front end decides how to show it.
  Install takes two steps, `inspect` and then `install`, so the manager can ask
  the user to confirm in between.
- **Install** (`bundle.rs`): core reads APKs and bundles (`.apks`, `.xapk`,
  `.apkm`) in-process, without the Android SDK. It filters ABI splits to the
  session's `ro.product.cpu.abilist`. One PackageInstaller session streams every
  APK through `install-write -`, so Android makes no extra copy.
- **APK metadata** (`apk/`): core parses the binary `AndroidManifest.xml` and
  the `resources.arsc` values it refers to, following AOSP's `ResourceTypes.h`.
  It picks a label for each locale the way Android does. The language and the
  script must match, and the closest region wins. A desktop entry's `Name[xx]`
  therefore matches the name the app shows. Core decodes only what an install
  uses. Malformed input returns an error and doesn't panic.
- **Icons:** the system app renders the launcher icon at sizes from 48 to 512 px
  for the bridge's app mirror. Adaptive and vector icons get the system mask.
  Before installation, `apk/icon.rs` renders static adaptive and vector layers
  on the host with resvg ([manager limits](../manager/README.md#install-page)).
- **Desktop entries** (`desktop.rs`, `xdg.rs`, `dynlauncher.rs`): each app's
  record, `dev.sargunv.innkeeper.<pkg>.desktop`, stays in the Flatpak's data
  dir. It lists the app's custom URL schemes as `x-scheme-handler/*`. The
  schemes `http`, `https`, `file`, `content` and `intent` are excluded. After
  the user confirms the DynamicLauncher portal's dialog, the record reaches the
  app menu as `dev.sargunv.Innkeeper.<pkg>.desktop`. That entry runs
  `flatpak run --command=innkeeper` for the entry's profile, which keeps working
  across updates. Windows (`StartupWMClass`), notifications and MPRIS players
  use the same id. The bridge's app mirror is the only writer of these entries,
  and `install` doesn't write them. The mirror takes names and URL schemes from
  the installed APK.
- **File types:** Innkeeper's own desktop entry claims only the APK and bundle
  types. `innkeeper open` hands documents to an **Open or share with Android
  app…** chooser ([the CLI](../cli/README.md)).
- **Profiles and adb** (`profiles.rs`, `adb.rs`): core lists, creates, renames,
  deletes, starts and stops profiles. It connects the user's adb server to a
  profile over adb's protocol on loopback.
- **Per-app settings** (`app_settings.rs`) and **preferences**
  (`preferences.rs`) go through the bridge's session D-Bus API. When no session
  runs, they are saved for the next start.
- **Paths** (`paths.rs`): core finds the prefix relative to the running
  executable, and the XDG dirs. A profile's own paths and settings come from
  `innkeeper-profile` (`host/profile`), which the launcher also uses.
- **Session** (`launcher.rs`): core starts the session on demand and waits for
  boot and user unlock. It reads whether the session runs and whether the image
  is ready from `innkeeper-profile`, in-process. Start, stop and `exec` run
  `innkeeper-session`. Commands inside Android are passed as argv and are never
  parsed by a shell.
- **Errors** (`errors.rs`): anyhow errors carry `ErrorKind` context, so front
  ends can pick user-facing text without parsing diagnostics. PackageManager
  install codes are parsed once, here, including codes that core doesn't know
  yet.
- **Untrusted input** (`validate.rs`): package names, labels, schemes, locales
  and URIs are validated before they reach a path, an entry or a command.

## Known limits

- The DynamicLauncher portal in xdg-desktop-portal 1.20 rejects
  `[Desktop Action]` groups and has no silent update. App-menu entries therefore
  have no actions, and repairing an entry shows the portal's dialog again.
