# launcher

`innkeeper-session` (crate `innkeeper-session`), staged to `libexec/innkeeper/`
by `mise run build:host`. It runs as the user inside the Innkeeper Flatpak,
builds nothing and finds the stage relative to itself. What the session is and
does is in [docs/session.md](../../docs/session.md); `--help` prints the usage.
Its library gives [host/core](../core/README.md) a profile's paths, settings and
status without running the launcher.

Subcommands: `start [-f] [--headless]`, `stop`, `status` (prints
`running: host pid N` or `stopped`), `prepare`, `image`, `reset`, `log [-f]`,
`exec CMD...`, `shell`, `selftest`. The internal `_serve` command owns the
session in a sandbox of its own.

`src/cli.rs` is the place to start; each module's header says what it owns.
`--help` lists the environment variables, and `INNKEEPER_VULKAN=lvp` runs
software Vulkan (lavapipe) instead of a render node, for debugging.

Tests: `cargo test -p innkeeper-session` (in `mise run test`), and
`mise run test:e2e -- boot`, which runs `selftest` in a fresh named profile.
Development sessions run through `tools/dev/session.sh`, the staged launcher in
a private Flatpak ([packaging/flatpak](../../packaging/flatpak/README.md)).

`session.log` records launcher milestones with elapsed milliseconds and host
`CLOCK_BOOTTIME` timestamps. Compare them with init's `ro.boottime.*` properties
and the event log's `boot_progress_*` timestamps to separate setup time from
Android's boot without reading host `dmesg`.
