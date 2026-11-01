# innkeeper-bridge

The desktop bridge is the only process that connects Android to the desktop. It
is `libexec/innkeeper/innkeeper-bridge`, built by `mise run build:host`. The
launcher starts it in the session's sandbox, so it ends when the session ends.
The launcher restarts it if it exits, and its clients reconnect.

## Services

| Id  | Service   | Android clients                         | Desktop side                                                                                                                                                                         | Code             |
| --- | --------- | --------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ---------------- |
| 1   | audio     | the audio HAL's `audio_bridge`          | PipeWire through its Pulse server (pure-Rust `pulseaudio` crate, no C deps)                                                                                                          | `src/audio/`     |
| 2   | display   | the composers (Android, OpenHarmony)    | Wayland: one `xdg_toplevel` per window, linux-dmabuf, viewporter, decorations                                                                                                        | `src/display/`   |
| 3   | system    | the system app (integration)            | portal Settings, `org.freedesktop.Notifications`, `ext_data_control_v1` / `wl_data_device`, OpenURI, Email, FileChooser SaveFile, MPRIS, and the session's activity for the launcher | `src/system/`    |
| 4   | documents | the system app (picker, provider)       | the FileChooser portal, parented through `zxdg_exporter_v2`                                                                                                                          | `src/documents/` |
| 5   | apps      | the system app (app mirror)             | the only writer of app entries, through `innkeeper-core`, with names and URL schemes from the installed APK, and the `Session1` relay (`src/session.rs`)                             | `src/apps/`      |
| 6   | input     | the system app only (WM, injector, IME) | the seat, its xkb keymap, gestures, tablets, cursor shape, `zwp_text_input_v3`                                                                                                       | `src/input/`     |
| 7   | a11y      | the system app only (accessibility)     | AT-SPI through AccessKit (`accesskit_unix`), one adapter per window                                                                                                                  | `src/a11y/`      |
| 8   | hoststate | the Health HAL, the system app          | UPower, the PowerProfileMonitor, NetworkMonitor and Location portals                                                                                                                 | `src/hoststate/` |

## Running

The header of `src/main.rs` documents the usage. The bridge takes no arguments.
It finds its profile's paths from `INNKEEPER_PROFILE` through `innkeeper-core`.
It reads these variables from the session's environment:

- `INNKEEPER_TEST=1` marks a test session
  ([docs/security.md](../../docs/security.md#test-sessions)).
- `INNKEEPER_AUDIO_SINK` and `INNKEEPER_AUDIO_SOURCE` pick the PipeWire sink and
  capture source.
- `INNKEEPER_BRIDGE_DEBUG=1` turns on debug logging and the frame and input
  trace.

The launcher adds `WAYLAND_DISPLAY`, `DBUS_SESSION_BUS_ADDRESS`,
`DBUS_SYSTEM_BUS_ADDRESS`, `XDG_DATA_HOME` (for mirrored desktop entries),
`XDG_CURRENT_DESKTOP` and `XDG_CONFIG_HOME` (for KDE's `plasma-localerc`).

[docs/bridge-protocol.md](../../docs/bridge-protocol.md) describes the protocol
and the rules every service follows. Each service's `proto.rs` module doc lists
its messages. `src/session.rs` serves and specifies the `Session1` D-Bus API
that the manager and the CLI use. `cargo test -p innkeeper-bridge` runs the
tests.

## Known limits

Clipboard text and images sync through `ext_data_control_v1` where the
compositor offers it. GNOME has no data control protocol, so the bridge uses the
display connection's `wl_data_device` and its keyboard-enter serial there. On
GNOME, sync works only while an Innkeeper window has focus, as with Waydroid. A
host copy arrives when an Innkeeper window regains focus. An Android copy made
without focus is offered at the next keyboard enter.

A file copy holds up to 16 regular files and preserves each file's name, MIME
type and bytes. Each file can be at most 16 MiB minus 1024 bytes. The whole
selection, including metadata, must fit in 16 MiB. A selection that is too large
or contains a directory is rejected in full. Directories are not traversed or
archived.

Android `content://` clips become temporary copies in the profile's cache. The
bridge offers them as `text/uri-list` and `x-special/gnome-copied-files`, plus a
FileTransfer portal key when the portal is available. A cache copy is removed
when a new copy replaces it and at the next bridge start. Host copies use the
portal grant when one is offered. A plain URI list works for files that the
Flatpak can already read.

Incoming files become private `content://` URIs of the system app. Android's
ClipboardService grants the pasting app read access. The bridge keeps only the
latest incoming file selection. Test sessions don't read or publish the desktop
clipboard on either backend.

Notification progress and badge counts use Unity LauncherEntry. Plasma, Ubuntu
Dock and Dash to Dock show it, and stock GNOME Shell doesn't. LauncherEntry has
no indeterminate state, so busy work shows at 50%, and several tasks show their
mean. Badges count the notifications that Launcher3 would show a dot for. They
don't count unread messages.

On Plasma, the bridge checks Do Not Disturb (`Inhibited`) before each post and
doesn't replay suppressed popups. On GNOME, the Shell applies its own policy,
and critical call and alarm notifications can bypass it.
