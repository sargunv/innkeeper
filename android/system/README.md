# system

`/system` binaries the session needs, built by `mise run build:system` and
staged under `share/linuxapk/android/system/`.

- **`exec/`: `linuxapk-exec`** (static C, NDK superbuild) and its rc. Binder
  works only for descendants of init, which inherit the seccomp filter, so
  processes that `linuxapk-session exec` enters with nsenter have no binder
  (`am`, `pm`, `cmd`). init runs `linuxapk-exec --serve`; the client hands it
  argv, environment, cwd and stdio fds over a root-only socket on the session's
  `/dev`, and it runs the command, as adbd does for `adb shell`.
- **`iptables/`:** upstream iptables with the nf_tables backend (`build.sh`, in
  the build container), because the image's legacy iptables needs kernel modules
  a rootless session can't load, while nf_tables works in the session's own
  network namespace. `linuxapk-iptables` stands in for the `ip{,6}tables` and
  `-restore` commands netd runs and drops `-m bpf` and `-m quota2` rules; its
  header says why.

Both are exercised by every boot (`mise run test:boot`) and the e2e suites,
which drive Android through `linuxapk-session exec`.
