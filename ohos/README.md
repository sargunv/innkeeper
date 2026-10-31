# ohos

An OpenHarmony userspace as a second guest, on the same supervisor as Android
(feasibility and milestones: #191). The pinned image
([image/ohos](../image/ohos/README.md)) boots in a test Flatpak sandbox on the
host kernel: OH's init starts its services, samgr comes up and OH's core system
abilities register with it. render_service composes OH's screen through our
display HDI ([vendor/display](vendor/display/README.md)), and with the bridge it
shows in a desktop window. Apps spawn and run their ArkTS code, but SceneBoard,
OH's window manager, doesn't start yet, so no app has a window; there is no
input or app install. Nothing here is part of the Android session,
`mise run build` or the stage.

| File                      | What                                                          |
| ------------------------- | ------------------------------------------------------------- |
| `build.sh`                | `mise run ohos:build`: the layer in `out/build/ohos/layer`    |
| `shim/ohos_shim.c`        | `liblinuxapk_ohos_shim.so`, preloaded into all of OH          |
| `sandbox.sh`              | inside the sandbox: OH's root, then pivot-exec runs OH's init |
| `run.py`                  | outside: a memory-capped dev sandbox, the boot log, probes    |
| `sys_prod/etc/init/*.cfg` | our init services, on OH's product partition                  |
| `vendor/display/`         | our composer and buffer VDIs, in place of the image's         |
| `tests/boot.sh`           | `mise run ohos:test`'s check, run inside OH                   |

`mise run build` first (the stage's pivot-exec and debugfs), then
`mise run ohos:test`: it fetches the image if needed and boots it as instance
`ohos` (scratch in `out/dev/flatpak/ohos/`, data wiped at each run). Init's log
is `ohos-kmsg.log` there; once hilogd runs, services log to hilog instead. To
look around, write a script for OH's `/system/bin/sh` and pass it with
`ohos/run.py --probe <script>`: it runs as root, with a native token holding
`ohos.permission.DUMP`, once `bootevent.samgr.ready` is set, and its output
comes back (`hidumper -ls`, `hilog -x`, `ps -A`).

## Display

```sh
LINUXAPK_INSTANCE=ohos tests/nested.sh ohos/run.py --display --timeout 60
```

`--display` also starts the bridge in the sandbox, so screen 0 opens as one
720x1280 window (log: `ohos-bridge.log`). Run it on a nested desktop
([docs/development.md](../docs/development.md#a-nested-isolated-desktop)), never
on your own.

- **GPU.** Only the first render node opens (the broker's DRM policy): the
  image's Mesa otherwise opens the host's card node, and its composer would
  drive the host display's KMS. That Mesa ships iris, crocus, nouveau,
  virtio_gpu and llvmpipe but no radeonsi, and CI has no GPU, so render_service
  draws with llvmpipe (`LIBGL_ALWAYS_SOFTWARE`). A GPU needs a Mesa built for
  OH's musl (#191's milestone 8).
- **Ashmem.** The HDI's command queues, like most OH shared memory, are ashmem,
  which the broker emulates with memfds
  ([supervisor/README.md](../supervisor/README.md#openharmony)).

## How it boots

The same path as Android's Flatpak mode
([docs/session.md](../docs/session.md#flatpak-mode)): a root of symlinks in the
sandbox's own `/`, then pivot-exec and the broker, then init.

- **Root.** `/system` is the extracted partition, `/vendor` its symlinks with
  our VDIs in place of the image's, and `/sys_prod` is our layer. OH's product
  partition is empty in the image but on init's cfg path and the musl linker's
  search path, so our files go there and the image is never changed. The sandbox
  keeps its `/usr`, `/bin` and `/etc` for the glibc side; `/etc` gains OH's
  other entries and `/lib` gains the musl loader.
- **Init.** `/system/bin/init --second-stage`: the image's first stage only
  mounts block devices and switches root. The shim makes `getpid()` 1 in init
  (it refuses to run otherwise), makes mounts and namespace calls no-ops
  (Flatpak's seccomp filter refuses them before ours sees them) and priority
  raises best-effort.
- **Apps.** appspawn gives each app a mount namespace: bind mounts into
  `/mnt/sandbox/<user>/<bundle>`, then `pivot_root` there, and apps find their
  own files only at those paths (`/data/storage/el1/bundle` is the app's code).
  In appspawn and the other spawners the shim makes those binds symlinks at
  their mount points, and `pivot_root` keeps the sandbox's `/data/storage` open
  on a fixed fd; the session's `/data/storage` links to `/proc/self/fd/<fd>`, so
  each app process sees its own. The rest of an app's root is the session's.
- **Broker.** The same broker, built static for musl (OH's root has no Bionic),
  with `LINUXAPK_GUEST=ohos`: it also traps `open(2)` (musl doesn't use
  `openat`), serves `/etc/passwd` and `/etc/group` from `/system/etc`, adds OH's
  binder ioctls and serves `/dev/access_token_id`
  ([supervisor/README.md](../supervisor/README.md#openharmony)).

Known gaps, visible in the logs and not fatal: no SELinux policy (the image
ships none), `mksandbox` can't `pivot_root`, ueventd's coldboot can't write the
read-only host sysfs, there are no cgroups or `/dev/memcg`, and hilogd reads
part of init's `/dev/kmsg`. SceneBoard doesn't start: BMS's home-ability queries
match nothing (it logs the legacy launcher as disabled in SceneBoard mode), so
UI abilities have no window manager.
