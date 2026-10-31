# innkeeper-bridge

The desktop bridge: the only process that connects Android to the desktop.
`libexec/innkeeper/innkeeper-bridge`, built by `mise run build:host`. The
launcher starts it in the session's sandbox (so the session's end ends it) and
restarts it if it exits; clients reconnect.

| Service   | Desktop side                                                                                                                                                                     | Code             |
| --------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------- |
| audio     | PipeWire through its Pulse server (pure-Rust `pulseaudio` crate, no C deps)                                                                                                      | `src/audio/`     |
| display   | Wayland: one `xdg_toplevel` per window, linux-dmabuf, viewporter, decorations                                                                                                    | `src/display/`   |
| input     | the seat, its xkb keymap, gestures, tablets, cursor shape, `zwp_text_input_v3`                                                                                                   | `src/input/`     |
| system    | portal Settings, `org.freedesktop.Notifications`, `ext_data_control_v1` / `wl_data_device`, OpenURI, Email, FileChooser SaveFile, MPRIS; the session's activity for the launcher | `src/system/`    |
| documents | the FileChooser portal, parented through `zxdg_exporter_v2`                                                                                                                      | `src/documents/` |
| apps      | desktop entries through `innkeeper-core`                                                                                                                                         | `src/apps/`      |
| a11y      | AT-SPI through AccessKit (`accesskit_unix`), one adapter per window                                                                                                              | `src/a11y/`      |
| hoststate | UPower, the PowerProfileMonitor, NetworkMonitor and Location portals                                                                                                             | `src/hoststate/` |

- **Usage:** the header of `src/main.rs`: it takes no arguments, finds its
  profile's paths from `INNKEEPER_PROFILE` through `innkeeper-core`, and reads
  `INNKEEPER_TEST=1` (a test session,
  [docs/security.md](../../docs/security.md#test-sessions)),
  `INNKEEPER_AUDIO_SINK`/`_SOURCE` and `INNKEEPER_BRIDGE_DEBUG=1` from the
  session's environment. The launcher adds `WAYLAND_DISPLAY`,
  `DBUS_SESSION_BUS_ADDRESS`, `DBUS_SYSTEM_BUS_ADDRESS`, `XDG_DATA_HOME`
  (mirrored desktop entries), `XDG_CURRENT_DESKTOP` and `XDG_CONFIG_HOME` (KDE's
  `plasma-localerc`).
- **Conventions:** a client can never block the bridge (events never wait, long
  work runs on service threads), decoders have focused protocol boundary tests,
  and shared memory the bridge hands out is sealed and never read back.

Protocol: [docs/bridge-protocol.md](../../docs/bridge-protocol.md); each
service's messages are its `proto.rs` module doc, and `src/session.rs` serves
the `Session1` D-Bus API for the manager and the CLI. Tests:
`cargo test -p innkeeper-bridge` (protocol, ring, a live server with hostile
clients).

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
