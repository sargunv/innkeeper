# host

The glibc binaries that run as the user in the Flatpak sandbox, outside
Android's seccomp filter.

| Dir                            | Artifact (stage path)                                               | Runs                                              | Start reading             |
| ------------------------------ | ------------------------------------------------------------------- | ------------------------------------------------- | ------------------------- |
| [core](core/README.md)         | `innkeeper-core` library (linked into cli, manager and bridge)      | in-process                                        | `src/ops.rs`              |
| [cli](cli/README.md)           | `bin/innkeeper`                                                     | from a shell or a desktop entry's `Exec`          | `src/main.rs`             |
| [manager](manager/README.md)   | `bin/innkeeper-manager`, `share/{applications,icons,metainfo,mime}` | from the app menu or as the APK handler           | `src/window.rs`           |
| [bridge](bridge/README.md)     | `libexec/innkeeper/innkeeper-bridge`                                | in the session's sandbox, started by the launcher | `src/main.rs`, `src/ipc/` |
| [launcher](launcher/README.md) | `libexec/innkeeper/innkeeper-session`, and a library core uses      | in the app's sandbox, then the session's own      | `src/cli.rs`              |

`mise run build:host` builds all of them (`tools/build/host.sh`). The CLI and
manager never touch Android directly: `innkeeper-core` drives the session
through `innkeeper-session exec` with argv only, and the bridge reuses core's
desktop-entry code for apps installed inside Android.
