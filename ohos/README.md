# ohos

An OpenHarmony userspace as a second guest, on the same supervisor as Android
(feasibility and milestones: #191). The pinned image
([image/ohos](../image/ohos/README.md)) boots in a test Flatpak sandbox on the
host kernel: OH's init starts its services under the supervisor and samgr comes
up. System abilities can't register yet: OH's binder extensions and access
tokens are missing. There is no display, input or app install yet, and nothing
here is part of the Android session, `mise run build` or the stage.

| File                      | What                                                          |
| ------------------------- | ------------------------------------------------------------- |
| `build.sh`                | `mise run ohos:build`: the layer in `out/build/ohos/layer`    |
| `shim/ohos_shim.c`        | `liblinuxapk_ohos_shim.so`, preloaded into OH's init          |
| `sandbox.sh`              | inside the sandbox: OH's root, then pivot-exec runs OH's init |
| `run.py`                  | outside: a memory-capped dev sandbox, the boot log, probes    |
| `sys_prod/etc/init/*.cfg` | our init services, on OH's product partition                  |
| `tests/boot.sh`           | `mise run ohos:test`'s check, run inside OH                   |

`mise run build` first (the stage's pivot-exec and debugfs), then
`mise run ohos:test`: it fetches the image if needed and boots it as instance
`ohos` (scratch in `out/dev/flatpak/ohos/`, data wiped at each run). Init's log
is `ohos-kmsg.log` there; once hilogd runs, services log to hilog instead. To
look around, write a script for OH's `/system/bin/sh` and pass it with
`ohos/run.py --probe <script>`: it runs as root once `bootevent.samgr.ready` is
set, and its output comes back (`hilog -x`, `ps -A`).

## How it boots

The same path as Android's Flatpak mode
([docs/session.md](../docs/session.md#flatpak-mode)): a root of symlinks in the
sandbox's own `/`, then pivot-exec and the broker, then init.

- **Root.** `/system` and `/vendor` are the extracted partitions; `/sys_prod` is
  our layer. OH's product partition is empty in the image but on init's cfg path
  and the musl linker's search path, so our files go there and the image is
  never changed. The sandbox keeps its `/usr`, `/bin` and `/etc` for the glibc
  side; `/etc` gains OH's other entries and `/lib` gains the musl loader.
- **Init.** `/system/bin/init --second-stage`: the image's first stage only
  mounts block devices and switches root. The shim makes `getpid()` 1 in init
  (it refuses to run otherwise), makes mounts and namespace calls no-ops
  (Flatpak's seccomp filter refuses them before ours sees them) and priority
  raises best-effort.
- **Broker.** The same broker, built static for musl (OH's root has no Bionic),
  with `LINUXAPK_GUEST=ohos`: it also traps `open(2)` (musl doesn't use
  `openat`) and serves `/etc/passwd` and `/etc/group` from `/system/etc`
  ([supervisor/README.md](../supervisor/README.md#openharmony)).

Known gaps, visible in the logs and not fatal: no SELinux policy (the image
ships none), `mksandbox` can't `pivot_root`, ueventd's coldboot can't write the
read-only host sysfs, there are no cgroups or `/dev/memcg`, and hilogd reads
part of init's `/dev/kmsg`.
