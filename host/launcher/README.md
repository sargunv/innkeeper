# launcher

`innkeeper-session` (crate `innkeeper-session`) is an internal tool. It is
staged to `libexec/innkeeper/` by `mise run build:host`. Users manage profiles
with `innkeeper profile`, including `reset` and `log`. Developers run the
launcher with `mise run session -- CMD`. It runs as the user inside the
Innkeeper Flatpak, builds nothing, and finds the stage relative to its own path.
[docs/session.md](../../docs/session.md) describes the session, and `--help`
prints the usage.

The launcher shares the `innkeeper-profile` crate (`host/profile`) with
[host/core](../core/README.md). That crate holds a profile's name, settings,
paths, status, activity report, control client and adb keys.

The subcommands are `start [-f] [--headless]`, `stop`, `status`, `prepare`,
`image`, `reset`, `log [-f]`, `exec CMD...`, `shell` and `selftest`. `status`
prints `running: host pid N` or `stopped`. `reset` keeps the profile's settings
file. The internal `_serve` command runs the session in its own sandbox.

Start reading at `src/cli.rs`. Each module's header says what it owns. A
profile's `system` setting picks the guest. For an OpenHarmony profile, the
image, root and init come from `src/ohos.rs`, and the rest of the code is
shared. `INNKEEPER_VULKAN=lvp` runs software Vulkan (lavapipe) in place of a
render node, for debugging.

`cargo test -p innkeeper-session` runs the unit tests, and `mise run test`
includes them. `mise run test:e2e -- boot` runs `selftest` in a fresh named
profile. Development sessions run through `tools/dev/session.sh`, which runs the
staged launcher in a private Flatpak
([packaging/flatpak](../../packaging/flatpak/README.md)).

`session.log` records launcher milestones with elapsed milliseconds and host
`CLOCK_BOOTTIME` timestamps. Compare them with init's `ro.boottime.*` properties
and the event log's `boot_progress_*` timestamps to separate setup time from
Android's boot time without reading the host's `dmesg`.
