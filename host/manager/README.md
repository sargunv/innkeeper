# innkeeper-manager

The manager is the Innkeeper app (`dev.sargunv.Innkeeper`), built with GTK 4 and
libadwaita. A user opens or drops an APK, sees what it is and which permissions
it wants, and installs it. A list then lets the user open, configure or remove
each app. `mise run build:host` builds `bin/innkeeper-manager` and its desktop
data in `data/`: the desktop entry, the metainfo, the icon and the MIME types
for `.apks`, `.xapk` and `.apkm`. `data/icons/dev.sargunv.Innkeeper.source.svg`
is the editable App Icon Preview source of the shipped icon.

Start reading at `src/window.rs`, which has the list, the session row and drag
and drop. Every core call runs on a worker thread (`src/worker.rs`), so reading
APKs, the session or the package manager doesn't block the UI thread.
`cargo test -p innkeeper-manager` tests the error, permission and progress
texts. For screenshots, use the nested desktop in
[docs/development.md](../../docs/development.md).

## Main window

- **APK handler:** the desktop entry runs `innkeeper-manager %F` and declares
  only the APK and bundle types, so "Open With" offers the manager for those
  files. `mise run package -- --install` installs the manager from a checkout's
  stage. The manager runs as a single instance. Opening another APK adds an
  install page to the running window.
- **First run** prepares the Android image and shows "Preparing Android".
- **The list** comes from the profile's desktop entries, so it shows without
  booting Android. A directory monitor picks up changes made with the CLI. At
  startup, the manager repairs stale entries the same way `innkeeper list` does.
  A row shows a spinner while its app opens or uninstalls. It shows **Open**
  while the app has windows.
- **Session state** follows D-Bus name ownership and the bridge's `State` and
  `Changed` API. This includes windows opened or closed outside the manager.
  Nothing polls.

## Install page

`src/install.rs` renders adaptive icons on the host without starting Android. It
handles static PNG and WebP layers, solid colours and vector paths (groups,
transforms, strokes and alpha), resolved from the APK's binary resources. It
applies a circular mask and crops the adaptive overscan. Some icons fall back to
a bitmap icon, and then to the generic icon. These include theme tints, colour
state lists, gradients, clip paths, trimmed paths, animated drawables, inset and
layer-list drawables, and resources that only another split supplies. Android
renders the icon after installation.

## App page

`src/app.rs` shows storage use and has controls to clear the cache or data,
runtime permission switches, Escape as Back, Run in Background, force stop and
uninstall. They go through the bridge's session D-Bus API
([session.rs](../bridge/src/session.rs)). Clearing data and uninstalling ask for
confirmation. Controls stay disabled until Android answers, and then every value
is read again. An error offers to read the values again.

## Profile Settings

`src/preferences.rs` (Ctrl+,) shows the settings of the profile in the window.
It reads them without starting Android. The page has these settings:

- **Display:** windowed, or a phone preset, used from the next start
  ([phone mode](../../docs/display.md#phone-mode)).
- **Desktop following:** the desktop's appearance, its language and region, and
  its clock. Each one is on by default.
- **Idle timeout.**
- **App-menu mirroring.**
- **adb:** a switch that applies immediately
  ([README](../../README.md#for-android-developers)).

The page also says that apps aren't isolated from each other, and it opens
Android Settings in a separate window.

- A profile keeps its settings in one settings file
  ([session.md](../../docs/session.md#profiles)). A running profile picks up
  changes through `Session1`'s `ReloadSettings`. Desktop following, mirroring
  and the idle timeout change while the profile runs. The display changes at the
  next start.
- Turning off desktop following keeps Android's current value. Later changes in
  Android Settings stay as they are. Turning off mirroring keeps existing
  launchers. Turning it back on adds apps installed in the meantime.
- By default, an idle Android stops after 2 minutes. The timeout can be 1 to 120
  minutes. **Keep Running** applies to every profile. With it on, no profile
  stops when idle, and one login command from the Background portal starts them
  all. If the portal denies the request, the setting stays off.

## Profiles

`src/profiles.rs` lists every profile and whether it runs. It creates, renames,
deletes, starts and stops profiles. The window shows one profile's apps, and the
profile's name is under the window title. **New Profile** asks for the profile's
system, Android or OpenHarmony. An OpenHarmony profile installs no apps yet, so
its window shows only its session and its settings show only its display. **Show
Apps** switches the window to another profile. The manager shows that profile
again the next time it opens. It stores the choice in
`$XDG_STATE_HOME/innkeeper/manager-profile`, and `--profile NAME` overrides it.
Opening the manager with no profile creates `default`. Deleting a profile asks
for confirmation and removes the profile's apps, data, settings and app-menu
entries.
