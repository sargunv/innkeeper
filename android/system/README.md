# system

`/system/bin` additions, built by `mise run build:system` and staged under
`share/innkeeper/android/system/`.

- **`exec/`: `innkeeper-exec`** (static C, NDK superbuild) and its rc. Binder
  works only for descendants of init, which inherit the seccomp filter, so
  processes that `innkeeper-session exec` starts in its sandbox have no binder
  (`am`, `pm`, `cmd`). init runs `innkeeper-exec --serve`; the client hands it
  argv, environment, cwd and stdio fds over a root-only socket on the session's
  `/dev`, and it runs the command, as adbd does for `adb shell`. Every boot and
  e2e group exercises it.
- **`iptables/`: `innkeeper-iptables`** and the `iptables`, `ip6tables` and
  `-restore` symlinks to it. Android shares the host's network, so netd's
  firewall commands succeed without changing anything; the `-restore` variants
  stay alive and echo netd's `#PING` acknowledgements.
