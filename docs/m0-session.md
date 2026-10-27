# M0a: the rootless session

Boots the Android 17 emulator image (`out/img`) under real Android `init` in a
rootless namespace on the dev host. Status: **M0a reached:
`sys.boot_completed=1` with no root and no AOSP patches**, about 4 s from zygote
start. system_server stays up with 289 binder services, and SystemUI and
Launcher3 run. See "What boots" and "Open issues".

## How to run

```sh
tools/extract-image.sh ~/Android/Sdk/system-images/android-37.0/default/x86_64 out/img  # once
session/linuxapk-session start        # first run: builds the shim and iptables, flattens APEXes
session/linuxapk-session start --headless   # no host Wayland window
session/linuxapk-session log -f       # init's /dev/kmsg (a FIFO read on the host)
session/linuxapk-session exec logcat -d -b main,system,crash   # (-b all can hang)
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

| Piece                                                                               | File                                                               |
| ----------------------------------------------------------------------------------- | ------------------------------------------------------------------ |
| APEX flattening (manifest name → `out/root/apex/<name>`, `apex-info-list.xml`)      | `tools/flatten-apex.sh`                                            |
| Launcher (transient systemd user service `linuxapk-session`, pasta, exec, selftest) | `session/linuxapk-session`                                         |
| Root assembly on tmpfs (runs as pid 1 of the outer namespaces)                      | `session/ns-init.sh`                                               |
| pivot_root, nested locked namespaces, fresh `/proc`, exec init                      | `session/helper/pivot-exec.c` (NDK, static)                        |
| LD_PRELOAD shim                                                                     | `session/shim/linuxapk_shim.c`                                     |
| Syscall emulation (bpf() and friends) via seccomp user notification                 | `session/helper/bpfemu.c`                                          |
| iptables with the nf_tables backend (NDK, podman), plus a `-m bpf` filter           | `session/iptables/`, `rootfs-overlay/system/bin/linuxapk-iptables` |
| Overrides layered over the image                                                    | `session/rootfs-overlay/{system,vendor,apex}`                      |
| Generated overlay layer (shim, relaxed-mode copies, vendor `build.prop`)            | `session/build.sh` → `out/session/`                                |

Namespaces: the outer `unshare` maps root to the host user, plus Android uids
1–29999, 50000–59999 and 90000–99999 onto `/etc/subuid`. `pivot-exec` then
clones init into nested user, mount, pid, cgroup, net, ipc and uts namespaces
with an identity map, so every inherited mount is locked (bubblewrap technique).
`/system`, `/vendor`, `/system_ext`, `/product`, `/odm` and the staged APEX tree
are read-only overlayfs mounts: repo overrides, then the display/GPU lanes'
`out/vendor-overlay` (vendor only) and the input lane's `out/system_ext-overlay`
(system_ext only), then generated files, then the image. Nothing in `out/img` is
modified.

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
- **eBPF is emulated in userspace.** pivot-exec installs a seccomp filter on
  init, inherited by every Android process. It turns `bpf(2)` into user
  notifications, which a pivot-exec thread answers (`helper/bpfemu.c`).
  - Maps (hash, array, per-CPU, LRU, LPM, local storage) live in that thread.
    Processes hold injected memfds that stand in for real object fds.
  - Pins are files on a session-private `/sys/fs/bpf` tmpfs.
  - Program load, attach and links succeed and do nothing; `PROG_QUERY` reports
    the attachments. Traffic accounting and the per-UID firewall are no-ops.
  - Because it works at the syscall level, there are no linker-namespace issues:
    netbpfload, the platform bpfloader, netd and system_server's JNI code are
    all covered.
  - The same supervisor also fakes:
    - `statfs` → `BPF_FS_MAGIC` for `/sys/fs/bpf` (libbpf pinning);
    - `getsockopt(SO_ANDROID_DROP_REASON)` (netd self-test);
    - raising `RLIMIT_MEMLOCK` (netd treats failure as fatal);
    - `security.selinux` reads for clatd and the BPF pins (ClatCoordinator
      verifies them);
    - `timerfd_create(CLOCK_*_ALARM)` → plain clocks;
    - SCHED_FIFO/RR requests.
  - The real netbpfload runs through `linuxapk-bpfload`, in a private mount
    namespace with writable stand-ins for the BPF sysctls.
- **iptables:** upstream iptables 1.8.11 with the nf_tables backend, which works
  in the session's own network namespace. It's layered over the image's legacy
  binary. `linuxapk-iptables` drops `-m bpf` rules, which is equivalent to
  programs that never match, and keeps netd's `#PING` handshake.
- **The shim also covers** `gzopen("/proc/config.gz")` (served from the host's
  `/boot/config-*`). The services that get it via rc `setenv` are
  servicemanager, zygote (and so system_server and apps), vold and installd.
- init.rc edits (diff against the committed stock copy): no ueventd (it can't
  open a uevent socket, so `ro.cold_boot_done` is set directly); binderfs
  without `stats=global`; `sys.use_memfd=true` (no ashmem). Other overrides: a
  cgroup2-only `cgroups.json`; netd1shot is a no-op; netd runs once; a fake
  `mmap_rnd_bits`. Image files whose fs_config owner was lost (`logd` 0550 and
  others) get world r/x copies. clatd gets its real owner and setuid bits
  through `podman unshare`, whose id map matches the session's.
- **Host devices and sockets:** `/dev/tun` (interfaces stay in the session's
  network namespace) and the host Wayland socket file as
  `/dev/socket/wayland-host`, plus `vendor.linuxapk.hwc.backend=wayland`. Both
  are only set up when not `--headless`.
- **Network for apps:** pasta names its tap interface `eth1`, and a vendor
  `android.hardware.ethernet` feature file makes system_server run
  EthernetService, which brings `eth1` up as the default network (DHCP from
  pasta). Without a network agent netd routes everything to `unreachable`, so
  apps were offline. `eth0` would come up restricted: the image's resources pin
  it for the emulator's radio. DHCP hands out `169.254.1.1` as the resolver
  (`-D --dhcp-dns`), and pasta forwards it to the host's first IPv4 nameserver
  (`--dns-host`; with `-D` set pasta stops picking it itself).
- **Defaults for apps:** gesture navigation
  (`rootfs-overlay/product/overlay/config/config.xml`): with three-button
  navigation, Launcher3's taskbar on this 1280x800 display crash-loops (NPE on
  `TaskbarControllers.cueBarController` from the bubble bar) whenever the home
  screen is in front. `debug.stagefright.c2-poolmask` adds BLOB, so Codec2
  linear buffers come from gralloc instead of ION/dma-buf heaps we can't open;
  without it every MediaCodec fails with `NO_MEMORY` (no game sound effects).
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
  `--no-map-gw`, so host loopback services are unreachable. The one exception is
  DNS: `169.254.1.1:53` is forwarded to the host's resolver. It also has its own
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

Everything needed for `sys.boot_completed=1` (`dev.bootcomplete=1`):

- logd, servicemanager, vold, keystore2 with software KeyMint, apexd, installd
  and statsd;
- netd (nf_tables iptables, emulated eBPF);
- SurfaceFlinger on the display lane's allocator/composer (ANGLE on RADV);
- zygote64 and system_server;
- SystemUI, Launcher3, Settings, the IME, Phone and PermissionController.

```
boot_progress_start → preload_end → system_run → pms_ready → ams_ready → enable_screen
sys.boot_completed=1, bpf.progs_loaded=1, init.svc.netd=running
selftest: 12/12 ok
```

## Open issues

- Bluetooth (`com.android.bluetooth`) crash-loops: there's no HCI HAL. It should
  be disabled via config.
- The emulator's system_ext apps (MultiDisplay, sdksetup) spam about missing
  goldfish pipes. Trim them.
- `logcat -b all` hangs; the specific buffers work.
- Emulated eBPF means no per-app traffic stats or firewall. VPNs and TUN work.
- Remaining interim stubs: the emulator audio HAL (until the PipeWire HAL lands)
  and the example vendor HALs.

## Vendor HALs needed for boot-completed

- **KeyMint, SecureClock, SharedSecret.** keystore2 sets
  `keystore.module_hash.sent`, which init.rc waits for. Gatekeeper is needed for
  gatekeeperd. The image's software HALs work.
- **graphics allocator, mapper and composer3.** SurfaceFlinger aborts without
  buffers, and its restarts take zygote down. EGL/Vulkan come through
  `ro.hardware.egl` and `ro.hardware.vulkan`.
- **audio core and effect.** AudioFlinger aborts without them. The emulator's
  HIDL 7.1 HAL works as a stub.
- Everything else is optional: health, power, thermal, lights, vibrator and the
  rest run as example HALs. Camera, sensors, radio, GNSS, Wi-Fi and BT are
  disabled.

## Blanked image parts (rootfs overlay)

- **Emulator apps:**
  `system_ext/priv-app/{MultiDisplayProvider,EmulatorRadioConfig}` are overlaid
  with empty APKs, which PackageManager skips. MultiDisplayProvider polled a
  missing goldfish device, used a full core and flooded logcat.
- **Bluetooth:** `vendor/etc/permissions/android.hardware.bluetooth{,_le}.xml`
  are overlaid with empty permission files, so system_server never starts the
  Bluetooth service. Without a Bluetooth HAL it crash-looped.
- **Logging:** `ro.logd.size=16M`.
