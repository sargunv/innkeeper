# linuxapk-bridge

The desktop bridge: the only process that connects Android to the desktop.
`libexec/linuxapk/linuxapk-bridge`, built by `mise run build:host`. The launcher
starts it inside the session's systemd unit (so it shares the memory cap and
`stop` ends it) and restarts it if it exits; clients reconnect.

| Service   | Desktop side                                                                                                                                                                     | Code             |
| --------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------- |
| audio     | PipeWire through its Pulse server (pure-Rust `pulseaudio` crate, no C deps)                                                                                                      | `src/audio/`     |
| display   | Wayland: one `xdg_toplevel` per window, linux-dmabuf, viewporter, decorations                                                                                                    | `src/display/`   |
| input     | the seat, its xkb keymap, gestures, tablets, cursor shape, `zwp_text_input_v3`                                                                                                   | `src/input/`     |
| system    | portal Settings, `org.freedesktop.Notifications`, `ext_data_control_v1` / `wl_data_device`, OpenURI, Email, FileChooser SaveFile, MPRIS; the session's activity for the launcher | `src/system/`    |
| documents | the FileChooser portal, parented through `zxdg_exporter_v2`                                                                                                                      | `src/documents/` |
| apps      | desktop entries through `linuxapk-core`                                                                                                                                          | `src/apps/`      |
| a11y      | AT-SPI through AccessKit (`accesskit_unix`), one adapter per window                                                                                                              | `src/a11y/`      |
| hoststate | UPower, the PowerProfileMonitor, NetworkMonitor and Location portals                                                                                                             | `src/hoststate/` |

- **Usage** is the header of `src/main.rs`: `--run-dir` and `--state-dir` are
  the instance's XDG runtime and state dirs (socket, MPRIS artwork, document
  grants, log), `--allow-uid` and `--system-app-dir` set who may connect, and
  `--test-session` (also `LINUXAPK_TEST=1`) makes a test session
  ([docs/security.md](../../docs/security.md#test-sessions)).
- **Environment:** `LINUXAPK_AUDIO_SINK` (one PipeWire sink),
  `LINUXAPK_BRIDGE_VERBOSE=1` (per-stream stats), and the caller's
  `WAYLAND_DISPLAY`, `DBUS_SESSION_BUS_ADDRESS`, `DBUS_SYSTEM_BUS_ADDRESS`,
  `XDG_CURRENT_DESKTOP` and `XDG_CONFIG_HOME` (KDE's `plasma-localerc`), which
  the launcher passes on.
- **Conventions:** a client can never block the bridge (events never wait, long
  work runs on service threads), decoders have focused protocol boundary tests,
  and shared memory the bridge hands out is sealed and never read back.

Protocol: [docs/bridge-protocol.md](../../docs/bridge-protocol.md); each
service's messages are its `proto.rs` module doc. Tests:
`cargo test -p linuxapk-bridge` (protocol, ring, a live server with hostile
clients); `examples/{tone,window,system,hoststate}.rs` drive a running bridge
the way the Android clients do.

## Known limits

Clipboard text and images sync through `ext_data_control_v1` where offered.
GNOME has no data control, so it uses the display connection's `wl_data_device`
and keyboard-enter serial: sync works only while a Linux.apk window has focus,
as with Waydroid. A host copy arrives when a Linux.apk window regains focus; an
Android copy made without focus is offered on the next keyboard enter.

File copies support up to 16 regular files per selection, preserving each name,
MIME type and bytes. Each file is capped at 16 MiB minus 1024 bytes; the
complete selection, including metadata, must fit in 16 MiB. Oversized selections
and selections containing directories are rejected in full; directories are not
traversed or archived. Android `content://` clips become temporary copies in the
instance's cache, offered as `text/uri-list` and `x-special/gnome-copied-files`,
plus a FileTransfer portal key when available. The cache copy is removed when
replaced and at the next bridge start. Host copies use the portal grant when
offered; a plain URI list works for files already accessible to the Flatpak.
Incoming files become private system-app `content://` URIs, with read access
granted by Android's ClipboardService to the pasting app. Only the latest
incoming file selection is retained. Test sessions never read or publish the
desktop clipboard, on either backend.

## GTK window experiment

An optional `gtk-spike` Cargo feature adds a development-only display backend.
Build with `cargo build --release -p linuxapk-bridge --features gtk-spike`, then
copy that binary into a development stage's `libexec/linuxapk/linuxapk-bridge`.
`LINUXAPK_GTK_SPIKE=1` selects it; without the variable the same binary uses the
existing Wayland frame. Normal builds do not link GTK. The launcher forwards
this variable to the bridge.

The experiment shows one Android window at a time in an `AdwApplicationWindow`,
with Back, fullscreen and a revealing `AdwToolbarView`. Frames are
`GdkDmabufTexture`s in a `GtkPicture` inside `GtkGraphicsOffload`; each texture
keeps its dma-buf and delays the composer's RELEASE until GDK drops it. It uses
GTK >= 4.14 and libadwaita >= 1.7. Run it only in a named disposable session,
using the isolation and memory caps in [AGENTS.md](../../AGENTS.md).

This is not a feature-complete backend: task appearance, activation tokens,
portal parenting, IME/compose, repeat, gestures, tablets and Android cursor
hotspot inference are unported. Keyboard forwarding currently uses physical keys
and basic modifiers. The native pointer-constraint objects borrow GDK's Wayland
handles and dispatch on its GLib thread. Benchmark traces mark GTK's
`before-paint` boundary and parent-surface presentation feedback explicitly;
those are not the raw backend's owned surface commits or physical photons.
