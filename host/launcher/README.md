# launcher

`linuxapk-session` (Rust, crate `linuxapk-session`), staged to
`libexec/linuxapk/` by `mise run build:host`. It runs as the user, builds
nothing and finds the stage relative to itself. What it does is in
[docs/session.md](../../docs/session.md); `--help` prints the usage. Its library
gives [host/core](../core/README.md) a session's paths, status and memory
without running it.

Subcommands: `start [-f] [--headless]`, `stop`, `status` (prints
`running: host pid N` or `stopped`, then `uid-mode=subuid|single`), `prepare`,
`image`, `reset`, `log [-f]`, `exec CMD...`, `shell`, `selftest`. The internal
ones are the processes that run a session: `_run` (the systemd unit), `_outer`
(makes the outer namespaces and forks their pid 1), `_serve` (Flatpak mode's
session in a sandbox of its own) and `_userns` (a command as Android's root, for
`reset` and the subuid derived layer).

| Module                             | What                                                                                  |
| ---------------------------------- | ------------------------------------------------------------------------------------- |
| `cli.rs`                           | the commands; start here                                                              |
| `paths.rs`, `conf.rs`              | every path, and `session.conf`: the one config `start` writes and the others read     |
| `uid.rs`                           | the uid mode, the id maps, entering the session's user namespace                      |
| `image.rs`, `layer.rs`             | preparing the image; the start-time layer, `/proc/cmdline` and `/proc/bootconfig`     |
| `host.rs`, `nsroot.rs`             | host mode: the unit, pasta, the outer namespaces and the root of overlays they mount  |
| `requirements.rs`, `appimage.rs`   | host mode: what the host lacks, named up front; running from the AppImage's copy      |
| `flatpak.rs`, `control.rs`         | Flatpak mode: the composed root, the sandbox's `/`, the session's lifetime and socket |
| `root.rs`, `init.rs`, `session.rs` | what both modes share: partitions, binder nodes, `/linuxapk`, pivot-exec, the bridge  |
| `exec.rs`, `selftest.rs`           | `exec` (namespaces or the control socket, then the exec relay) and the probes         |
| `lifecycle.rs`                     | stopping when idle, and the wakeup for the next alarm                                 |

| Knob                                               | For                                                             |
| -------------------------------------------------- | --------------------------------------------------------------- |
| `LINUXAPK_AUDIO_SINK`                              | users: one PipeWire sink for the session's audio                |
| `LINUXAPK_IDLE_GRACE`                              | users: seconds idle before the session stops (0: never)         |
| `--headless`, `LINUXAPK_INSTANCE`                  | tests: no windows; an independent named (test) session          |
| `LINUXAPK_OPEN_URLS`                               | tests: let a test session open links                            |
| `LINUXAPK_APPIMAGE`                                | set by the AppImage's `AppRun`: desktop entries run through it  |
| `LINUXAPK_DATA`, `LINUXAPK_IMAGES`, `LINUXAPK_PIN` | tests: Android's `/data`, the image cache, the pin              |
| `LINUXAPK_SUBUID_FILE`, `LINUXAPK_SUBGID_FILE`     | tests: stand-ins for `/etc/sub[ug]id` when picking the uid mode |
| `LINUXAPK_BRIDGE_VERBOSE`, `LINUXAPK_BROKER_DEBUG` | debugging: bridge stream stats; broker tracing                  |

Tests: `cargo test -p linuxapk-session` (in `mise run test`: subordinate id
ranges, the uid mode a `/data` keeps, the pin, property merging, mountinfo and
apex-info parsing) and `mise run test:boot`, which boots a fresh headless
instance in each uid mode and runs `selftest`.
