# `linuxapk-gui`: the Linux.apk manager

A desktop app in the spirit of Gear Lever: open or drop an APK, check what it
is, install it, then open or remove it from a list. It's Rust with GTK 4 and
libadwaita (`gui/`), a thin front end over the same library as the `linuxapk`
CLI. The CLI stays the scriptable path.

```sh
cargo build --release -p linuxapk -p linuxapk-gui
target/release/linuxapk-gui [App.apk]      # opens the install page for a file
linuxapk setup                             # APK files open in the GUI; adds it to the app menu
```

Building needs the GTK 4 (4.18+) and libadwaita (1.7+) development files
(`pkg-config gtk4 libadwaita-1`); this Fedora 44 host has them, and its runtime
libraries (GTK 4.22, libadwaita 1.9). Without them, build in a rootless
container (`registry.fedoraproject.org/fedora:44` plus `gtk4-devel`,
`libadwaita-devel`, `cargo`). Like the CLI, the build embeds the icon helper, so
it also wants `javac` and the Android SDK.

## Architecture

```
 cli/  linuxapk        clap, stdout/stderr, --notify      ┐
 gui/  linuxapk-gui    GTK 4 + libadwaita, worker threads ├─► core/  linuxapk-core
                                                          ┘     ops: inspect, install, uninstall,
                                                                     run, list, installed_apps, setup
                                                                session, android, manifest (aapt2),
                                                                bundle, desktop, xdg, validate
```

- **`core/` (`linuxapk-core`)** holds everything that used to be in `cli/src`,
  plus `ops`, the user-level operations both front ends call. Operations report
  progress through a callback (`Event::Status`/`Warning`), not stderr. Install
  has two steps, so the GUI can show a confirmation in between:
  - `ops::inspect(file)` reads the APK or bundle with aapt2, with no session
    needed. It returns the label, version, permissions, native ABIs, size and an
    icon, and it holds the unpacked bundle until the install.
  - `ops::install(&package)` installs it into the session, starting the session
    if needed.
- **`cli/`** is clap plus printing. Its output and tests are unchanged, apart
  from one new progress line ("adding it to the app menu").
- **`gui/`**: `window.rs` (list, session, drag and drop), `install.rs` (the
  confirmation page), `settings.rs`, `errors.rs` (messages for people),
  `permissions.rs` (plain names), `worker.rs`. Every core call runs on a
  `gio::spawn_blocking` thread. Its progress events come back over an async
  channel to the UI thread, which never blocks on aapt2, the session or the
  package manager.

## What it does

- **Install.** Use "Open APK…" (Ctrl+O), drop files on the window, or open an
  APK from the file manager. The install page shows:
  - the icon, name, package and version;
  - the size (and APK count for a bundle) and the native ABIs;
  - permissions in plain words, personal-data ones first with a warning icon.
    Permissions capped below Android 16 (`maxSdkVersion`) are left out.

  "Install" changes to "Reinstall" or "Replace" if this instance already has the
  app. An app with only ARM code gets a banner and no install button. The steps
  then show in place ("Starting Android…", "Installing…", "Adding to the app
  menu…"), and the page can't be left until it finishes. A toast offers "Open".
  The page's icon is the APK's own largest bitmap launcher icon. An adaptive
  (XML) icon falls back to the legacy PNGs of the same resource (found with
  `aapt2 dump resources`), else the manager's icon. Android renders the real
  icon at install, as in the CLI.

- **Installed apps** come from this instance's desktop entries (the
  `LINUXAPK_INSTANCE` in their `Exec`), so the list shows without booting
  Android. While the session runs, user packages without an entry are added. A
  directory monitor refreshes the list when the CLI installs or removes
  something. Each row has Open (`ops::run`: a window of its own through
  `dev.linuxapk.LAUNCH`) and Uninstall, which asks first and offers "Keep App
  Data" (`cmd package uninstall -k`).
- **Session:** running, starting or stopped; memory is the unit's cgroup
  `MemoryCurrent`. It's polled every 3 s off the UI thread. Start runs
  `Session::ensure` (start and wait for boot) and Stop runs
  `linuxapk-session stop`.
- **Preferences:**
  - "Open APK Files With Linux.apk" switches the handler (below).
  - Placeholders for M3: follow dark style, accent color, text size, and stop
    when idle.
  - The session instance and launcher path.
- **Errors** are an alert (or the install page itself) with a heading and a
  sentence for known cases: not an APK, ARM only, signature conflict, downgrade,
  an older Android, damaged APK, missing aapt2, Android not starting, no
  launcher activity. Other errors get a generic heading. The raw error chain
  sits under "Details".

## APK handler and registration (decision)

`linuxapk setup` registers the GUI when it is installed: `linuxapk-gui` next to
`linuxapk`, or on `PATH`. Otherwise it registers the CLI as before.

| file                                                   | GUI installed                                                                                          | CLI only                                        |
| ------------------------------------------------------ | ------------------------------------------------------------------------------------------------------ | ----------------------------------------------- |
| `applications/dev.linuxapk.installer.desktop`          | `Exec=linuxapk-gui %f`, `Icon=dev.linuxapk.Manager`, `NoDisplay`, the APK MIME types; default for them | `Exec=linuxapk install --notify %f` (unchanged) |
| `applications/dev.linuxapk.Manager.desktop`            | the app-menu entry "Linux.apk" (`Exec=linuxapk-gui`, `StartupWMClass=dev.linuxapk.Manager`)            | not written                                     |
| `icons/hicolor/scalable/apps/dev.linuxapk.Manager.svg` | the manager's icon (`core/data/`)                                                                      | not written                                     |
| `mime/packages/dev.linuxapk.xml`                       | `.apks`, `.xapk`, `.apkm` types                                                                        | same                                            |

- The handler keeps one desktop ID either way, so `mimeapps.list` defaults
  survive a switch between the GUI and the CLI.
- The GUI is a single instance (`GApplication`, app ID `dev.linuxapk.Manager`,
  `HANDLES_OPEN`). Opening an APK while it runs pushes a new install page into
  the open window.
- `setup --remove` removes the handler and the manager's menu entry. The
  Preferences switch turns the handler on (GUI handler plus menu entry) or off
  (handler only). The menu entry stays, because you're using it.
- Both IDs have one segment after `dev.linuxapk.`, so no Android package (two or
  more segments) can collide with them.
- Entries carry `LINUXAPK_INSTANCE`/`LINUXAPK_SESSION` if they were set, as the
  CLI's do. The app ID doesn't, so one GUI process serves one instance.

## Flatpak (later)

The binary runs on the host today. For Flathub it builds against
`org.gnome.Platform//50` (installed here) with `flatpak-builder --user`. Inside
the sandbox the session and aapt2 must run on the host: user namespaces can't be
nested in Flatpak (see STRATEGY M5). The plan:

- `finish-args: --talk-name=org.freedesktop.Flatpak` (for now; a narrower portal
  later), `--filesystem=xdg-data/applications:create` and
  `xdg-data/icons:create` for the entries.
- When `/.flatpak-info` exists, core's `Session` and `Aapt2` run their argv
  through `flatpak-spawn --host`, with the same argv-only rule. Desktop entries
  then `Exec` the host `linuxapk`, which must be installed on the host.
- Open: shipping the host-side launcher. It's a shell script plus our
  out-of-tree vendor files, so it would come from a host package or a one-time
  "install the session" step.

## Testing

- `cargo test -p linuxapk-core -p linuxapk -p linuxapk-gui`:
  - the error messages and permission names;
  - progress texts;
  - permission parsing (with `maxSdkVersion`);
  - the legacy-bitmap lookup for adaptive icons;
  - `Exec` instance parsing and the desktop-app listing;
  - `inspect` on the OpenCalc corpus APK (icon, permissions).
- `cli/tests/integration.sh` passes unchanged on the shared library
  (`LINUXAPK_INSTANCE=gui`, 2026-10-27).
- Screens without a desktop to spare (or with the screen locked): run a nested
  `kwin_wayland --virtual` as in [windowing.md](windowing.md) "Testing", start
  the GUI with that `WAYLAND_DISPLAY` and `DBUS_SESSION_BUS_ADDRESS`
  (`GTK_A11Y=none`, since the nested bus has no a11y), and capture with
  `spectacle -b -n -a`. Clicks used KWin fake input.

### Proof (2026-10-27, nested KWin 6.7, instance `gui`)

- Install through the GUI: OpenCalc, then Wikipedia (opened as the handler
  would, `linuxapk-gui <file>`; Install clicked). Session start, install, icon,
  and a valid entry (`X-LinuxApk-Version` added), then the list and an "Open"
  toast.
- Launch through the GUI: "Open" on each row opened Calculator and Wikipedia in
  windows of their own (`dev.linuxapk.<pkg>` app IDs in KWin).
- Uninstall through the GUI with "Keep App Data": the entry and icons went away
  and `/data/data/<pkg>` stayed.
- Errors: a text file named `.apk` shows "Not an Android App". ARM-only Open
  Sudoku shows the banner with Install disabled.

## Open issues

- Adaptive icons with no legacy bitmaps show the manager's icon on the install
  page. Rendering them on the host needs vector-drawable support (or Android
  rendering an uninstalled APK while the session runs).
- No per-row progress for open and uninstall (a toast only). The list doesn't
  show which apps have open windows (`dev.linuxapk.LIST` could).
- The M3 settings are placeholders until the bridge/system-app channel lands.
- Flatpak packaging (above).
