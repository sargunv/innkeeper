# linuxapk-bridge

The desktop bridge: the only process that connects Android to the desktop.
`libexec/linuxapk/linuxapk-bridge`, built by `mise run build:host`. The launcher
starts it inside the session's systemd unit (so it shares the memory cap and
`stop` ends it) and restarts it if it exits; clients reconnect.

| Service   | Desktop side                                                                            | Code             |
| --------- | --------------------------------------------------------------------------------------- | ---------------- |
| audio     | PipeWire through its Pulse server (pure-Rust `pulseaudio` crate, no C deps)             | `src/audio/`     |
| display   | Wayland: one `xdg_toplevel` per window, linux-dmabuf, viewporter, decorations           | `src/display/`   |
| input     | the seat, its xkb keymap, gestures, tablets, cursor shape, `zwp_text_input_v3`          | `src/input/`     |
| system    | portal Settings, `org.freedesktop.Notifications`, `ext_data_control_v1`, OpenURI, MPRIS | `src/system/`    |
| documents | the FileChooser portal, parented through `zxdg_exporter_v2`                             | `src/documents/` |
| apps      | desktop entries through `linuxapk-core`                                                 | `src/apps/`      |
| a11y      | AT-SPI through AccessKit (`accesskit_unix`), one adapter per window                     | `src/a11y/`      |
| hoststate | UPower, power-profiles-daemon, NetworkMonitor or NetworkManager, the Location portal    | `src/hoststate/` |

- **Usage** is the header of `src/main.rs`: `--run-dir` and `--state-dir` are
  the instance's XDG runtime and state dirs (socket, MPRIS artwork, document
  grants, log), `--allow-uid` and `--system-app-dir` set who may connect, and
  `--mirror-apps`, `--no-open-urls` and `--no-desktop-presence` are what make a
  test session ([docs/security.md](../../docs/security.md#test-sessions)).
- **Environment:** `LINUXAPK_AUDIO_SINK` (one PipeWire sink),
  `LINUXAPK_BRIDGE_VERBOSE=1` (per-stream stats), and the caller's
  `WAYLAND_DISPLAY`, `DBUS_SESSION_BUS_ADDRESS`, `DBUS_SYSTEM_BUS_ADDRESS`,
  `XDG_CURRENT_DESKTOP` and `XDG_CONFIG_HOME` (KDE's `plasma-localerc`), which
  the launcher passes on.
- **Conventions:** a client can never block the bridge (events never wait, long
  work runs on service threads), every decoder is bounded and proptested, and
  shared memory the bridge hands out is sealed and never read back.

Protocol: [docs/bridge-protocol.md](../../docs/bridge-protocol.md); each
service's messages are its `proto.rs` module doc. Tests:
`cargo test -p linuxapk-bridge` (protocol, ring, a live server with hostile
clients); `examples/{tone,window,system,hoststate}.rs` drive a running bridge
the way the Android clients do.
