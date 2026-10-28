# launcher

`linuxapk-session` and `ns-init.sh` (bash), staged to `libexec/linuxapk/` by
`mise run build:host`. `linuxapk-session` runs as the user and builds nothing;
it finds the stage relative to itself. `ns-init.sh` runs as pid 1 of the
session's outer namespaces and assembles Android's root. Inside a Flatpak both
run in the app's sandbox instead, with no namespaces or mounts
([Flatpak mode](../../docs/session.md#flatpak-mode)): `ns-init.sh` composes the
root on disk and links it into the sandbox's `/`, and `start` stays in the
foreground. What they do is in [docs/session.md](../../docs/session.md); the
usage is the header of `linuxapk-session` (also its `--help` output).

Subcommands: `start [-f] [--headless]`, `stop`, `status` (prints
`uid-mode=subuid|single`), `prepare`, `image`, `reset`, `log [-f]`,
`exec CMD...`, `shell`, `selftest`. `start` writes `session.conf` into the
instance's runtime dir; `ns-init.sh` sources it.

| Knob                                               | For                                                             |
| -------------------------------------------------- | --------------------------------------------------------------- |
| `LINUXAPK_AUDIO_SINK`                              | users: one PipeWire sink for the session's audio                |
| `--headless`, `LINUXAPK_INSTANCE`                  | tests: no windows; an independent named (test) session          |
| `LINUXAPK_OPEN_URLS`                               | tests: let a test session open links                            |
| `LINUXAPK_DATA`, `LINUXAPK_IMAGES`, `LINUXAPK_PIN` | tests: Android's `/data`, the image cache, the pin              |
| `LINUXAPK_SUBUID_FILE`, `LINUXAPK_SUBGID_FILE`     | tests: stand-ins for `/etc/sub[ug]id` when picking the uid mode |
| `LINUXAPK_BRIDGE_VERBOSE`, `LINUXAPK_BROKER_DEBUG` | debugging: bridge stream stats; broker tracing                  |

Tests: `host/launcher/tests/uid-mode.sh` (in `mise run test`) and
`mise run test:boot`, which boots a fresh headless instance in each uid mode and
runs `selftest`.
