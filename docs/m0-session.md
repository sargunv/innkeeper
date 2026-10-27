# M0a: the rootless session

Boots the Android 17 emulator image (`out/img`) under real Android `init` in a
rootless namespace on the dev host. Status: **system_server runs to
`StartNetworkManagementService` (82 system services started, PackageManager
ready) and then blocks on netd.** See "Blockers".

## How to run

```sh
tools/extract-image.sh ~/Android/Sdk/system-images/android-37.0/default/x86_64 out/img  # once
session/linuxapk-session start        # builds the shim, flattens APEXes on first run
session/linuxapk-session log -f       # init's /dev/kmsg (a FIFO read on the host)
session/linuxapk-session exec logcat -d
session/linuxapk-session exec getprop sys.boot_completed
session/linuxapk-session shell
session/linuxapk-session selftest     # host-safety probes; all must say "ok"
session/linuxapk-session stop
```

`LINUXAPK_HOLD=1 session/linuxapk-session start` assembles the root but runs a
shell loop instead of init (for poking at the tree). `/data` persists in
`~/.local/share/linuxapk/data`. Its files belong to subordinate uids, so wipe it
with `podman unshare rm -rf ~/.local/share/linuxapk/data`.

## How it works

| Piece                                                                               | File                                          |
| ----------------------------------------------------------------------------------- | --------------------------------------------- |
| APEX flattening (manifest name → `out/root/apex/<name>`, `apex-info-list.xml`)      | `tools/flatten-apex.sh`                       |
| Launcher (transient systemd user service `linuxapk-session`, pasta, exec, selftest) | `session/linuxapk-session`                    |
| Root assembly on tmpfs (runs as pid 1 of the outer namespaces)                      | `session/ns-init.sh`                          |
| pivot_root, nested locked namespaces, fresh `/proc`, exec init                      | `session/helper/pivot-exec.c` (NDK, static)   |
| LD_PRELOAD shim                                                                     | `session/shim/linuxapk_shim.c`                |
| Overrides layered over the image                                                    | `session/rootfs-overlay/{system,vendor,apex}` |
| Generated overlay layer (shim, relaxed-mode copies, vendor `build.prop`)            | `session/build.sh` → `out/session/`           |

Namespaces: the outer `unshare` maps root to the host user, plus Android uids
1–29999, 50000–59999 and 90000–99999 onto `/etc/subuid`. `pivot-exec` then
clones init into nested user, mount, pid, cgroup, net, ipc and uts namespaces
with an identity map, so every inherited mount is locked (bubblewrap technique).
`/system`, `/vendor`, `/system_ext`, `/product`, `/odm` and the staged APEX tree
are read-only overlayfs mounts: repo overrides, then the display/GPU lanes'
`out/vendor-overlay` (vendor only), then generated files, then the image.
Nothing in `out/img` is modified.

Decisions:

- **init second stage directly** (`/system/bin/init second_stage`). The launcher
  does first stage's job: it mounts `/dev`, `/proc`, `/sys`, and passes
  `androidboot.*` through a fake `/proc/bootconfig`
  (`androidboot.hardware=linuxapk`).
- **SELinux hidden.** `/sys/fs/selinux` is an empty tmpfs, so libselinux reports
  SELinux as disabled. The shim fixes the init call sites that still assume a
  policy: `security_compute_create`, `set*con`, the `security.selinux` xattr
  (bionic property area), and `selinux_status_*` (servicemanager CHECKs it). It
  also clamps `setrlimit` and makes `setpriority` best-effort. It is preloaded
  into init only, by absolute path. init unsets `LD_PRELOAD` for its children,
  because no single path resolves from every linker namespace. servicemanager
  and zygote get it via `setenv` in their rc files.
- **No apexd mounting.** Init's tmpfs over `/apex` is covered by a bind of the
  staged tree in early-init. `linkerconfig` runs from init as usual. apexd still
  serves `apexservice`: a wrapper (`/system/bin/linuxapk-apexd`, private mount
  namespace) adds erofs "file-backed mount" entries to the `/proc/mounts` that
  apexd reads, so it reports all 50 APEXes active, and PackageManager finds the
  apks-in-apex.
- init.rc edits (diff against the committed stock copy): no ueventd (it can't
  open a uevent socket, so `ro.cold_boot_done` is set directly); binderfs
  without `stats=global`; `sys.use_memfd=true` (no ashmem). Other overrides: a
  cgroup2-only `cgroups.json`; `bpfloader` and `netd1shot` as no-ops; netd runs
  once; a fake `mmap_rnd_bits`. Image files whose fs_config owner was lost
  (`logd` 0550 and others) get world r/x copies.
- **Vendor:** `init.linuxapk.rc` replaces `init.ranchu.rc`. The goldfish
  services are blanked: graphics, camera, sensors, radio, gnss, wifi, bt, c2,
  usb, fingerprint and uwb rc files, plus empty VINTF fragments. The ranchu
  allocator and hwc3 fragments conflicted with the display lane's HALs and broke
  the whole device manifest. The emulator's legacy audio HAL (HIDL 7.1,
  primary/r_submix) is kept for now: it registers without devices, and without
  any audio HAL audioserver aborts. Graphics props come from
  `out/vendor-overlay/etc/gpu.prop` and are merged into `/vendor/build.prop`:
  RADV if the render node is amdgpu, else lavapipe.

## Host safety

See NOTES.md "Host safety". `selftest` checks the following:

- The nested user namespace locks every inherited mount. The sysfs masks,
  `/dev/kmsg` and the APEX tree can't be unmounted; `/sys` can't be remounted
  rw; fresh sysfs mounts are refused; and no FUSE connections are visible.
- Android has its own network namespace through pasta: no inbound forwards, and
  `--no-map-gw`, so host loopback services are unreachable. It also has its own
  IPC and UTS namespaces, which isolates host abstract unix sockets and SysV
  shm.
- All processes run in `linuxapk-session.service`'s delegated cgroup, and the
  cgroup namespace root is that unit. `exec` joins it too. Host supplementary
  groups are dropped.
- The only writable host paths are `/data` (`~/.local/share/linuxapk/data`) and
  the kmsg FIFO. Host device nodes: null, zero, full, random, urandom, fuse,
  udmabuf and `/dev/dri`.
- vold stays enabled, because boot needs it (vdc calls, StorageManager). Its
  host-reaching paths are now dead ends: `AbortFuseConnections` hits an empty
  read-only tmpfs; process kills only reach the session pid namespace; and
  FITRIM, loop/dm and init's `FS_IOC_SHUTDOWN` need init-namespace capabilities.
  zygote's `onrestart` runs `vdc volume abort_fuse` on every zygote restart,
  which is how the incident repeated.

## What boots

These start and stay up: logd, servicemanager, hwservicemanager, vold, keystore2
with software KeyMint, gatekeeperd, apexd, statsd, installd, tombstoned,
credstore, gpuservice, traced, media services, audioserver (on the emulator
audio HAL), the display lane's allocator and composer3, and **SurfaceFlinger**
(ANGLE on RADV). Example vendor HALs also stay up: health, power, thermal,
lights, vibrator, identity and others. zygote64 preloads and forks
system_server:

```
boot_progress_system_run → pms_start → pms_system_scan_start → pms_data_scan_start → pms_ready
SystemServerTiming: StartNetworkManagementService
NetdService: WARNING: returning null INetd instance.   (repeats every second)
```

## Blockers

1. **netd (hard wall, needs a decision).** netd aborts in
   `libnetd_updatable_init()` because the network eBPF programs can't be loaded:
   `unprivileged_bpf_disabled=2`, and pinning needs bpffs. With that init
   stubbed (experimental `session/shim/netd_shim.c`), netd then exits on
   `iptables-restore: unable to initialize table 'filter'`. The host has
   nf_tables only, and a user namespace can't autoload the legacy x_tables
   modules. system_server waits for INetd forever. Past netd,
   ConnectivityService and NetworkStats (Tethering APEX) also require eBPF maps.
   Options:
   - (a) A one-time root setup: load the `ip_tables`/`ip6_tables` modules, and
     delegate a bpffs through a BPF token (kernel ≥ 6.9).
   - (b) A stub INetd plus patched connectivity eBPF users. That means an AOSP
     patch and a rebuilt Tethering APEX, from the image-builder lane.
   - (c) Accept root, as Waydroid does.
2. **AlarmManager / timerfd:** `timerfd_create(CLOCK_*_ALARM)` gets EPERM
   (CAP_WAKE_ALARM in the initial namespace). Seen in audioserver; it hasn't
   blocked anything yet.
3. **Interim cross-lane override:**
   `rootfs-overlay/vendor/etc/init/android.hardware.graphics.allocator-linuxapk.rc`
   runs the allocator as root. Only the session's root maps to the host user,
   who has the logind ACL on `/dev/udmabuf`. The HAL should open udmabuf before
   dropping privileges, after which this override can go.

## Vendor HALs needed so far

Each of these is required by something that blocks or aborts without it:

- **KeyMint, SecureClock, SharedSecret.** keystore2 sets
  `keystore.module_hash.sent`, and init.rc waits for it. Gatekeeper is needed
  for gatekeeperd. The image's software versions work.
- **graphics allocator, mapper and composer3.** SurfaceFlinger aborts without
  buffers, and its restarts kill zygote. EGL/Vulkan through `ro.hardware.egl`
  and `ro.hardware.vulkan`.
- **audio core and effect HAL.** AudioFlinger aborts in `onFirstRef` without
  one. The emulator's HIDL 7.1 HAL is enough as a stub.
- Health, power and the rest are optional so far: their example HALs run, and
  system_server hasn't reached the services that use them.
