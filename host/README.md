# host

Everything that runs on the user's desktop: glibc binaries and scripts that run
as the user, outside Android's namespaces.

| Dir                            | Artifact (stage path)                                              | Runs                                                   | Start reading             |
| ------------------------------ | ------------------------------------------------------------------ | ------------------------------------------------------ | ------------------------- |
| [core](core/README.md)         | `linuxapk-core` library (linked into cli, manager and bridge)      | in-process                                             | `src/ops.rs`              |
| [cli](cli/README.md)           | `bin/linuxapk`                                                     | from a shell or a desktop entry's `Exec`               | `src/main.rs`             |
| [manager](manager/README.md)   | `bin/linuxapk-manager`, `share/{applications,icons,metainfo,mime}` | from the app menu or as the APK handler                | `src/window.rs`           |
| [bridge](bridge/README.md)     | `libexec/linuxapk/linuxapk-bridge`                                 | in the session's systemd unit, started by the launcher | `src/main.rs`, `src/ipc/` |
| [launcher](launcher/README.md) | `libexec/linuxapk/linuxapk-session`, and a library core uses       | as the user, then as pid 1 of the outer namespaces     | `src/cli.rs`              |

`mise run build:host` builds all of them (`tools/build/host.sh`). The CLI and
manager never touch Android directly: `linuxapk-core` drives the session through
`linuxapk-session exec` with argv only, and the bridge reuses core's
desktop-entry code for apps installed inside Android.
