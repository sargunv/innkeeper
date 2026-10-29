# launcher

`linuxapk-session` (Rust, crate `linuxapk-session`), staged to
`libexec/linuxapk/` by `mise run build:host`. It runs as the user, builds
nothing and finds the stage relative to itself. What it does is in
[docs/session.md](../../docs/session.md); `--help` prints the usage. Its library
gives [host/core](../core/README.md) a session's paths, status and memory
without running it.

Subcommands: `start [-f] [--headless]`, `stop`, `status` (prints
`running: host pid N` or `stopped`), `prepare`, `image`, `reset`, `log [-f]`,
`exec CMD...`, `shell`, `selftest`. The internal `_serve` command owns the
session in a sandbox of its own.

| Module                             | What                                                                                                |
| ---------------------------------- | --------------------------------------------------------------------------------------------------- |
| `cli.rs`                           | the commands; start here                                                                            |
| `paths.rs`, `conf.rs`              | every path, and `session.conf`: the one config `start` writes and the others read                   |
| `image.rs`, `layer.rs`             | preparing the image; the start-time layer, `/proc/cmdline` and `/proc/bootconfig`                   |
| `flatpak.rs`, `control.rs`         | Flatpak mode: the composed root, the sandbox's `/`, the session's lifetime and socket               |
| `root.rs`, `init.rs`, `session.rs` | partitions, binder nodes and helpers: partitions, binder nodes, `/linuxapk`, pivot-exec, the bridge |
| `exec.rs`, `selftest.rs`           | `exec` (control socket, then the exec relay) and the probes                                         |
| `lifecycle.rs`                     | stopping when idle, and the wakeup for the next alarm                                               |

| Knob                                               | For                                                                |
| -------------------------------------------------- | ------------------------------------------------------------------ |
| `LINUXAPK_AUDIO_SINK`                              | users: one PipeWire sink for the session's audio                   |
| `LINUXAPK_IDLE_GRACE`                              | users: seconds idle before the session stops (0: never)            |
| `--headless`, `LINUXAPK_INSTANCE`                  | tests: no windows; an independent named (test) session             |
| `LINUXAPK_TEST_TIMERS`                             | tests: short idle polling and alarm wake lead, named sessions only |
| `LINUXAPK_OPEN_URLS`                               | tests: let a test session open links                               |
| `LINUXAPK_DATA`, `LINUXAPK_IMAGES`, `LINUXAPK_PIN` | tests: Android's `/data`, the image cache, the pin                 |
| `LINUXAPK_BRIDGE_VERBOSE`, `LINUXAPK_BROKER_DEBUG` | debugging: bridge stream stats; broker tracing                     |

Tests: `cargo test -p linuxapk-session` (in `mise run test`: the pin, property
merging and apex-info parsing), and `mise run test:e2e -- boot`, which runs
`selftest` in a fresh named sandbox. Sessions under development run through
`tools/dev/session.sh`, the staged launcher in a private Flatpak
([packaging/flatpak](../../packaging/flatpak/README.md)).
