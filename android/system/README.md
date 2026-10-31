# system

`/system` binaries the session needs, built by `mise run build:system` and
staged under `share/innkeeper/android/system/`.

- **`exec/`: `innkeeper-exec`** (static C, NDK superbuild) and its rc. Binder
  works only for descendants of init, which inherit the seccomp filter, so
  processes that `innkeeper-session exec` starts in its sandbox have no binder
  (`am`, `pm`, `cmd`). init runs `innkeeper-exec --serve`; the client hands it
  argv, environment, cwd and stdio fds over a root-only socket on the session's
  `/dev`, and it runs the command, as adbd does for `adb shell`. The relay is
  exercised by every boot (`mise run test:e2e -- boot`) and the e2e suites,
  which drive Android through `innkeeper-session exec`.
