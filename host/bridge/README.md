# innkeeper-bridge

The desktop bridge: the only process that connects Android to the desktop.
`libexec/innkeeper/innkeeper-bridge`, built by `mise run build:host`. The
launcher starts it in the session's sandbox (so the session's end ends it) and
restarts it if it exits; clients reconnect.

## Services

| Id  | Service   | Android clients                         | Desktop side                                                                                                                                                                     | Code             |
| --- | --------- | --------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------- |
| 1   | audio     | the audio HAL's `audio_bridge`          | PipeWire through its Pulse server (pure-Rust `pulseaudio` crate, no C deps)                                                                                                      | `src/audio/`     |
| 2   | display   | the composers (Android, OpenHarmony)    | Wayland: one `xdg_toplevel` per window, linux-dmabuf, viewporter, decorations                                                                                                    | `src/display/`   |
| 3   | system    | the system app (integration)            | portal Settings, `org.freedesktop.Notifications`, `ext_data_control_v1` / `wl_data_device`, OpenURI, Email, FileChooser SaveFile, MPRIS; the session's activity for the launcher | `src/system/`    |
| 4   | documents | the system app (picker, provider)       | the FileChooser portal, parented through `zxdg_exporter_v2`                                                                                                                      | `src/documents/` |
| 5   | apps      | the system app (app mirror)             | the only writer of app entries, through `innkeeper-core`, with names and URL schemes from the installed APK; the `Session1` relay (`src/session.rs`)                             | `src/apps/`      |
| 6   | input     | the system app only (WM, injector, IME) | the seat, its xkb keymap, gestures, tablets, cursor shape, `zwp_text_input_v3`                                                                                                   | `src/input/`     |
| 7   | a11y      | the system app only (accessibility)     | AT-SPI through AccessKit (`accesskit_unix`), one adapter per window                                                                                                              | `src/a11y/`      |
| 8   | hoststate | the Health HAL, the system app          | UPower, the PowerProfileMonitor, NetworkMonitor and Location portals                                                                                                             | `src/hoststate/` |

## Running

- **Usage:** the header of `src/main.rs`: it takes no arguments, finds its
  profile's paths from `INNKEEPER_PROFILE` through `innkeeper-core`, and reads
  `INNKEEPER_TEST=1` (a test session,
  [docs/security.md](../../docs/security.md#test-sessions)),
  `INNKEEPER_AUDIO_SINK`/`_SOURCE` and `INNKEEPER_BRIDGE_DEBUG=1` from the
  session's environment. The launcher adds `WAYLAND_DISPLAY`,
  `DBUS_SESSION_BUS_ADDRESS`, `DBUS_SYSTEM_BUS_ADDRESS`, `XDG_DATA_HOME`
  (mirrored desktop entries), `XDG_CURRENT_DESKTOP` and `XDG_CONFIG_HOME` (KDE's
  `plasma-localerc`). Protocol:
  [docs/bridge-protocol.md](../../docs/bridge-protocol.md), including the rules
  every service follows; each service's messages are its `proto.rs` module doc,
  and `src/session.rs` serves and specifies the `Session1` D-Bus API for the
  manager and the CLI. Tests: `cargo test -p innkeeper-bridge`.

## Known limits

Clipboard text and images sync through `ext_data_control_v1` where offered.
GNOME has no data control, so it uses the display connection's `wl_data_device`
and keyboard-enter serial: sync works only while an Innkeeper window has focus,
as with Waydroid. A host copy arrives when an Innkeeper window regains focus; an
Android copy made without focus is offered on the next keyboard enter.

File copies support up to 16 regular files per selection, preserving each name,
MIME type and bytes. Each file is capped at 16 MiB minus 1024 bytes; the
complete selection, including metadata, must fit in 16 MiB. Oversized selections
and selections containing directories are rejected in full; directories are not
traversed or archived. Android `content://` clips become temporary copies in the
profile's cache, offered as `text/uri-list` and `x-special/gnome-copied-files`,
plus a FileTransfer portal key when available. The cache copy is removed when
replaced and at the next bridge start. Host copies use the portal grant when
offered; a plain URI list works for files already accessible to the Flatpak.
Incoming files become private system-app `content://` URIs, with read access
granted by Android's ClipboardService to the pasting app. Only the latest
incoming file selection is retained. Test sessions never read or publish the
desktop clipboard, on either backend.

Notification progress and badge counts use Unity LauncherEntry, which Plasma and
Ubuntu Dock / Dash to Dock show and stock GNOME Shell doesn't. It has no
indeterminate state, so busy work shows at 50%, and several tasks show their
mean; badges count notifications Launcher3 would dot, not unread messages. On
Plasma the bridge honours Do Not Disturb (`Inhibited`) before each post and
doesn't replay suppressed popups; on GNOME the Shell applies its own policy, and
critical call and alarm notifications can bypass it.
