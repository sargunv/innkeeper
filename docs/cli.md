# `linuxapk`: the host command

`linuxapk` (Rust, `cli/`) is the user-facing host command. It installs Android
apps into the session and gives them desktop entries and icons, launches them,
and wraps the session launcher. It runs on the host (glibc) and drives Android
only through `session/linuxapk-session exec`.

## Usage

```sh
cargo build --release -p linuxapk          # -> target/release/linuxapk
linuxapk install App.apk                   # also .apks, .xapk, .apkm
linuxapk run org.wikipedia                 # launch (starts the session if needed)
linuxapk run org.wikipedia https://en.wikipedia.org/wiki/Linux
linuxapk list                              # user-installed apps
linuxapk uninstall org.wikipedia [--keep-data]
linuxapk session start [--headless] | stop | status
linuxapk setup [--remove]                  # double-click APKs to install them
```

After `install`, the app is in the desktop's app menu ("Calculator", with its
own icon). Its custom URL schemes open in it: `xdg-open wikipedia://…` launches
Wikipedia. `linuxapk setup` registers an "open with" handler for APK files and
bundles, so double-clicking one in Dolphin installs it and shows the progress as
notifications.

`install`, `run`, `uninstall` and `list` start the session if it isn't running
(with its host window; `LINUXAPK_HEADLESS=1` for none) and wait for boot.
Building needs `javac` and the Android SDK (for the embedded icon helper);
running `install` needs the SDK's `aapt2`.

## What `install` does

1. **Reads the input.** A plain APK is used in place. A bundle is unpacked into
   `~/.cache/linuxapk/install-<pid>/` with generated names (`0.apk`, …) and size
   caps, then deleted. From a bundletool `.apks` it takes `splits/` (else
   `universal.apk`); from `.xapk`/`.apkm` it takes every APK. OBB files are
   skipped with a warning.
2. **Inspects every APK with `aapt2 dump badging`** (`$LINUXAPK_AAPT2`, else the
   newest `$ANDROID_HOME/build-tools/*/aapt2`, default SDK `~/Android/Sdk`).
   There must be exactly one base APK, and every split must name the same
   package. ABI config splits (`config.arm64_v8a`, …) are installed only for
   ABIs in the session's `ro.product.cpu.abilist`. It fails early if nothing
   matches.
3. **Installs** in one PackageInstaller session: `cmd package install-create`,
   then one `install-write -S <size> … -` per APK, streamed through stdin (no
   copy inside the session), then `install-commit`. On failure it runs
   `install-abandon`. Reinstalling replaces the app and keeps its data.
4. **Asks Android for the icon.** The embedded helper
   (`cli/android/IconDump.java`, compiled to dex by `cli/build.rs`) is copied to
   `/data/local/tmp/linuxapk-icon.jar` and run under `app_process`. It loads the
   launcher activity's icon at xxxhdpi (an `activity-alias` icon wins, as in
   Launcher3) and draws it at 48, 64, 128, 256 and 512 px. The framework does
   the rendering, so adaptive icons get the system mask and vector drawables
   just work. If that fails, the APK's own PNG launcher icons are used.
5. **Reads the manifest** (`aapt2 dump xmltree`). It takes
   `appCategory`/`isGame` for Categories, plus intent filters of exported
   activities and aliases with `VIEW` + `BROWSABLE` and no `mimeType`. Custom
   schemes become `MimeType=x-scheme-handler/<scheme>;`. `http`/`https` (and
   `file`, `content`, `intent`, `javascript`, …) are **never** declared. The
   hosts of `http(s)` deep links are only recorded as `X-LinuxApk-Web-Hosts`,
   for a future URL router.
6. **Writes** `~/.local/share/icons/hicolor/<n>x<n>/apps/dev.linuxapk.<pkg>.png`
   and `~/.local/share/applications/dev.linuxapk.<pkg>.desktop` (atomic renames;
   old icons are removed first). It then runs `gtk-update-icon-cache` (only if
   that theme dir already has a cache) and `update-desktop-database`.

The entry:

```ini
[Desktop Entry]
Type=Application
Version=1.5
Name=Wikipedia
Name[de]=…                       # from the APK's localized labels
Comment=Android app (org.wikipedia)
Icon=dev.linuxapk.org.wikipedia
Exec=linuxapk run org.wikipedia %U
Terminal=false
StartupWMClass=dev.linuxapk.org.wikipedia
Categories=Utility;
MimeType=x-scheme-handler/wikipedia;
Keywords=Android;
X-LinuxApk-Package=org.wikipedia
X-LinuxApk-Web-Hosts=*.wikipedia.org;
```

`Exec` is `linuxapk` if that's what `PATH` finds, else the binary's absolute
path. If `LINUXAPK_INSTANCE` or `LINUXAPK_SESSION` were set at install time,
they are baked in (`env LINUXAPK_INSTANCE=cli … run …`), so a lane's entries
keep using its session.

## `run`, `uninstall`, `list`, `setup`

- **`run <pkg> [uri]`.** Without a URI it resolves the launcher activity
  (`cmd package resolve-activity`) and starts it with `MAIN`/`LAUNCHER` and
  `NEW_TASK|RESET_TASK_IF_NEEDED`, as Launcher3 does, so a running app comes to
  the front. With a URI it sends `VIEW` + `BROWSABLE` (the URI comes from an
  untrusted origin, so only activities that accept browser links get it)
  restricted to the package. Both go through `android::launch`, the single place
  that decides how to start an app. Today that is
  `cmd activity start-activity --display 0`.
- **`uninstall <pkg> [--keep-data]`** only uninstalls packages listed by
  `pm list packages -3`, so system apps are refused. It runs
  `cmd package uninstall [-k]`, then removes the entry and every
  `hicolor/*/apps/dev.linuxapk.<pkg>.png`.
- **`list`** prints the user packages with the `Name` of their entry.
- **`setup`** writes `dev.linuxapk.installer.desktop`
  (`Exec=… install --notify %f`, `NoDisplay`) for
  `application/vnd.android.package-archive` plus three types it defines in
  `~/.local/share/mime/packages/dev.linuxapk.xml`: `application/x-android-apks`
  (`*.apks`), `-xapk` and `-apkm`. It then runs `update-mime-database` and
  `xdg-mime default`. `setup --remove` deletes both files and drops the handler
  from `mimeapps.list`.

## Safety

APKs, and URIs passed to `run`, are untrusted input.

- **No shell.** Host tools (aapt2, notify-send, xdg-mime, …) and Android
  commands get argv only. `linuxapk-session exec` ends in `exec "$@"`. The one
  `sh -c` (copying the helper jar) is a constant script with a constant path.
- **Package names** must match Android's grammar (dot-separated segments, each a
  letter followed by `[A-Za-z0-9_]`, at least two segments, at most 255 bytes)
  before they're used in a path, desktop ID or command.
- **Labels** are trimmed and limited to 128 chars. A label containing a control
  character or bidi override is rejected, and the package name is used instead
  (a localized label is just dropped). Values are then escaped per the Desktop
  Entry spec (`\s \n \t \r \\`, `\;` in lists). Exec arguments are quoted per
  the spec (reserved characters, `%%`).
- **Schemes** must be lowercase RFC 3986 schemes of at most 64 chars and not on
  the deny list. Web hosts must match `(*.)?[a-z0-9.-]+`. Locale keys must be
  `ll` or `ll_CC`.
- **URIs** for `run` must have a scheme and contain no spaces or control
  characters. `file:`, `content:`, `intent:`, `javascript:` and
  `android.resource:` are refused. The URI is always the value of `-d`.
- **Notifications** escape markup in the body.
- **Zip entry names** never become paths, and extraction is size-capped.

## Tests

- `cargo test -p linuxapk` covers desktop entry generation and escaping, Exec
  quoting, package, scheme, label, locale and URI validation, badging and
  xmltree parsing, bundle member selection and ABI filtering, `mimeapps.list`
  editing, and the icon and entry files in a temp `XDG` tree. Corpus fixtures
  (Wikipedia, OpenCalc, Thunderbird, Shattered Pixel Dungeon) are read by path
  from `$LINUXAPK_CORPUS` (default the compat lane's `compat/out/corpus`).
  They're skipped if absent, and never committed.
- `cli/tests/integration.sh` runs against a real session
  (`LINUXAPK_INSTANCE=cli` by default). It installs OpenCalc and Wikipedia, runs
  `desktop-file-validate` (via podman if the tool is missing) and checks icon
  sizes, the idempotent reinstall and `list`. It launches OpenCalc with
  `gtk-launch` and Wikipedia with `xdg-open wikipedia://…`, checks the `dumpsys`
  top resumed activity, checks that bad input is refused, and uninstalls with a
  check that nothing is left. `KEEP=1` skips the uninstall.

## Windowing integration (M2)

Today every app shares display 0's one "Linux.apk" window (app id `linuxapk`).
Two changes switch to per-app windows:

- **`android::launch`** becomes a call to the windowing lane's control command:
  "start this intent on its own display/window". `Launch::Main { component }`
  and `Launch::View { package, uri }` already carry what it needs.
- **The composer** gives each app's `xdg_toplevel` the app id
  `dev.linuxapk.<package>`, matching `StartupWMClass` and the desktop-file ID,
  so the taskbar groups windows under the app's own icon and name.
