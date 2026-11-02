# innkeeper-manager

The manager is the Innkeeper app (`dev.sargunv.Innkeeper`), built with GTK 4 and
libadwaita. A user opens or drops an APK, sees what it is and which permissions
it wants, and installs it. A list then lets the user open, configure or remove
each app. `mise run build:host` builds `bin/innkeeper-manager` and its desktop
data in `data/`: the desktop entry, the metainfo, the icon and the MIME types
for `.apks`, `.xapk` and `.apkm`. `data/icons/dev.sargunv.Innkeeper.source.svg`
is the editable App Icon Preview source of the shipped icon.

Start reading at `src/window.rs`, which has the header, the app list and drag
and drop. Every core call runs on a worker thread (`src/worker.rs`), so reading
APKs, the session or the package manager doesn't block the UI thread.
`cargo test -p innkeeper-manager` tests the error, permission and progress
texts. For screenshots, use the nested desktop in
[docs/development.md](../../docs/development.md).

## Layout

The design follows Gear Lever: one list, a page per item, and one way to do each
thing.

- **The header** has **+** to install an app, the profile switcher in the
  middle, and the main menu with **Profile Settings** (Ctrl+,) and **About**.
- **The profile switcher** shows the window's profile. Its menu lists every
  profile and **New Profile…**. It is the only place to switch or create a
  profile.
- **The list** shows the profile's apps. A row opens the app's page.
- **App and install pages** share a top: the icon, name, package and version
  above the page's buttons (`src/hero.rs`). While an operation runs, its
  progress takes the buttons' place.
- **Profile Settings** is the only place for everything else about a profile:
  whether it runs, its name, its settings, and deleting it.

## Main window

- **APK handler:** the desktop entry runs `innkeeper-manager %F` and declares
  only the APK and bundle types, so "Open With" offers the manager for those
  files. `mise run package -- --install` installs the manager from a checkout's
  stage. The manager runs as a single instance. Opening another APK adds an
  install page to the running window.
- **Download:** a profile whose system isn't downloaded yet shows **Download
  Android** (or OpenHarmony) with the download's size, in place of its app list,
  so an OpenHarmony user never fetches Android. The download starts when the
  user asks (`src/download.rs`). It shows a progress bar with the size, speed
  and time left, then a spinner while the image is unpacked. Core reads the
  progress from the size of image/fetch.sh's partial download. A file opened
  before the image is ready opens its install page once it is.
- **The list** comes from the profile's desktop entries, so it shows without
  booting Android. A directory monitor picks up changes made with the CLI. At
  startup, the manager repairs stale entries the same way `innkeeper list` does.
- **Session state** follows D-Bus name ownership and the bridge's `State` and
  `Changed` API. Nothing polls.

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

`src/app.rs` has **Open** and **Force Stop** at the top. Below are storage use
with buttons that clear the data or the cache, runtime permission switches,
Escape as Back, Run in Background, the types the app opens by default, and
**Uninstall**. They go through the bridge's session D-Bus API
([session.rs](../bridge/src/session.rs)). Clearing data and uninstalling ask for
confirmation. Controls stay disabled until Android answers, and then every value
is read again. An error offers to read the values again. OpenHarmony doesn't
report app settings, so an OpenHarmony app's page has Open and Uninstall.

## Profile Settings

`src/profile.rs` (Ctrl+,) shows the window's profile in a dialog. It reads the
settings without starting Android. The dialog has these rows:

- **The profile's state:** stopped, starting or running, with Start or Stop.
- **Name:** renames a stopped profile.
- **Display:** windowed, or a device window that Android's screen resizes with,
  used from the next start ([phone mode](../../docs/display.md#phone-mode)).
  OpenHarmony shows as a phone of a fixed size.
- **Follow the Desktop:** the desktop's appearance, its language and region, and
  its clock. Each one is on by default. OpenHarmony has no accent color.
- **When No Apps Are Open:** stop after an idle timeout, or Keep Running.
- **adb:** a switch that applies immediately
  ([README](../../README.md#for-android-developers)). OpenHarmony profiles have
  an **hdc** switch in its place
  ([README](../../README.md#for-openharmony-developers)). While it is on, its
  row shows how to connect.
- **Android Settings** opens in a separate window. The same group says that apps
  aren't isolated from each other.
- **Delete Profile** asks for confirmation, stops the profile if it runs, and
  removes the profile's apps, data, settings and app-menu entries. The window
  then shows the first profile left, and opening the manager with no profile
  creates `default`.

A profile keeps its settings in one settings file
([session.md](../../docs/session.md#profiles)). A running profile picks up
changes through `Session1`'s `ReloadSettings`. Desktop following and the idle
timeout change while the profile runs. The display changes at the next start,
and changing it while the profile runs says so in a toast. Turning off desktop
following keeps the guest's current value. Later changes in Android's or
OpenHarmony's Settings stay as they are.

By default, an idle profile stops after 2 minutes. The timeout can be 1 to 120
minutes. A profile with **Keep Running** on never stops when idle, and starts at
login. If the Background portal denies starting at login, the setting stays off.

## Profiles

**New Profile** asks for a name and a system, Android or OpenHarmony, and
switches the window to the new profile. The manager shows the same profile the
next time it opens. It stores the choice in
`$XDG_STATE_HOME/innkeeper/manager-profile`, and `--profile NAME` overrides it.
An OpenHarmony profile's window lists the apps that have desktop entries.
