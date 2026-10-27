# Orchestrator notes

Working state for whoever (human or agent) picks this up. Keep this file short:
current state, decisions, next steps. Put history in git.

## Ground rules

- Push straight to `main` (no PRs). The repo is public (since 2026-10-27), with
  no CI for now. Post nothing outside this repo.
- Follow the strategy in [STRATEGY.md](STRATEGY.md). Record any pivots here and
  update STRATEGY.md.
- Dev host: Fedora 44 x86_64, KDE Wayland, AMD Strix Halo, 32 threads, 128 GB.
  The `linuxapk_dev` SELinux module is installed, and binder `set_context_mgr`
  works unprivileged.
- No sudo. Build deps go in rootless podman.

## Host safety (incident 2026-10-26)

Inside our user namespace, Android "root" is the host user, so it can touch
anything the host user owns. vold's shutdown path wrote to
`/sys/fs/fuse/connections/*/abort` and killed every host FUSE mount, including
T3's AppImage, which crashed T3 with SIGBUS.

Rules:

- Every namespace that runs Android code must cover all host sysfs submounts and
  make `/sys` read-only (see `session/ns-init.sh`).
- Run sessions in their own delegated systemd user unit, never in the caller's
  cgroup.
- Don't bind-mount host `/run/user`, `$HOME` or `/dev` wholesale.
- Done (session lane): Android runs in nested user/mount/pid/net/ipc/uts
  namespaces, so the masks are locked against umount; pasta gives it its own
  network namespace. `session/linuxapk-session selftest` checks all of this.

## Host memory incident (2026-10-27 01:37)

The kernel's global OOM killer and systemd-oomd crashed the user's desktop apps.
Causes:

- the AOSP build (siso about 23 GB plus parallel R8/javac, inside an 80 GB cap);
- `/tmp`, a RAM tmpfs, holding about 42 GB, including our 3.5 GB logcat dump
  `/tmp/lc.txt`;
- swap full. Fixes: deleted our `/tmp` files; session unit capped at 6G/8G; AOSP
  container capped at 32 GB with `-j12`. The AOSP build is **paused** until it's
  actually needed (M4: our own redistributable image). The rules are in
  AGENTS.md.

- Main session moved to the CI image 2026-10-27. The old emulator-image `/data`
  is kept at `~/.local/share/linuxapk/data-emulator-backup-20261027` (delete it
  with `podman unshare rm -rf`).

## Locations

- Repo: `~/Projects/linux.apk`
- AOSP checkout (android-17.0.0_r1): `~/aosp/a17` (outside the repo)
- Android image: `~/.local/share/linuxapk/images/<name>/` (`image/pin.toml`,
  `tools/fetch-image.sh`); SDK emulator image (optional):
  `~/Android/Sdk/system-images/android-37.0/default/x86_64`
- Large working files: `~/Projects/linux.apk/out/` (gitignored)

## Findings (graphics)

- AOSP 17 `external/` includes mesa3d (26.1-devel; Android.bp builds only
  lavapipe, zink and gfxstream, not RADV), minigbm (intel/msm/meson variants, no
  amdgpu), drm_hwcomposer (has an hwc3 AIDL frontend), angle, swiftshader,
  wayland and wayland-protocols, all buildable for Android.
- RADV uses ACO and needs no LLVM, so it can be cross-built out of tree with
  meson and the NDK. ANGLE (already in /system) provides GLES on top of
  RADV/lavapipe Vulkan.
- Device access for this user: `/dev/dri/renderD128` (world rw), and
  `/dev/udmabuf` and `card1` (logind ACL). There's no access to
  `/dev/dma_heap/system`. So memfd → udmabuf gives CPU-backed dma-bufs for a
  simple gralloc.

## Plan to M4 (decided 2026-10-27)

- **M2 windowing:** one Android trusted virtual display per app task, created by
  the Linux.apk System app. Our composer presents each display as its own
  `xdg_toplevel` (HWC-backed virtual displays preferred). A window resize calls
  `VirtualDisplay.resize`, and input carries a display id.
- **M2 audio:** a Rust host-side `bridge/` (glibc, outside the namespaces)
  reached through a socket bound into the session. Audio flows from Android
  through a shared memfd ring to PipeWire. Interim HAL: a legacy
  `audio.primary.linuxapk.so` under the image's HIDL 7.1 wrapper. The source
  image gets a proper AIDL HAL.
- **M3:** desktop integration in the bridge plus the system app. The host
  `linuxapk` CLI (Rust) installs APKs and generates `.desktop` files.
- **M4 ubinder:** binder emulated through the session's seccomp
  user-notification supervisor (the one already used for bpf). openat on binder
  devices gets an injected memfd (`SECCOMP_IOCTL_NOTIF_ADDFD`), and the broker
  services `BINDER_*` ioctls through the target's memory. No AOSP patches, and
  it covers every linker namespace. Virtual uids come from the broker.
- **M4 scope:** ubinder as the default, virtual uids (no subuid range needed),
  our own source-built image, and corpus parity. Relocating `/system` and
  `/apex` paths is only needed inside Flatpak, where nested user namespaces are
  blocked. Its feasibility is assessed in M4 and implemented in M5.
- **Image source (2026-10-27):** boot an AOSP build from Android's public CI
  instead of the emulator image (SDK license) or our own source build.
  - Branch `aosp-android-latest-release`, build 16644040, target
    `aosp_cf_x86_64_only_phone-userdebug` (CP2A.260605.016, test-keys).
  - Users download it from ci.android.com at setup, so we never redistribute it.
  - Its vendor APEXes supply the software KeyMint, gatekeeper, health and AIDL
    audio HALs.
  - The local AOSP build **finished** 2026-10-27 03:44 (`generic_x86_64`
    system.img; tree in `~/aosp/a17`, about 240 GB). It's kept as a fallback and
    for test binaries (binder tests). Ask the user before deleting it once the
    project is proven without it.
- Concurrency: at most three agent lanes plus the capped AOSP build. Each lane
  uses its own `LINUXAPK_INSTANCE`.

## Lanes (worktrees in `~/Projects/linux.apk-wt/<lane>`, branch `lane/<lane>`, rebase then push to main)

- session: namespace launcher, APEX flattening, init boot (M0a). Docs:
  `docs/m0-session.md`.
- hal: allocator, mapper (stable-C v5), composer3, built with the NDK and
  installed into `out/vendor-overlay`. Docs: `docs/hal-display.md`.
- gpu: RADV plus a software Vulkan driver, cross-built with the NDK into
  `out/vendor-overlay`. Docs: `docs/gpu.md`.
- win (M2, done): per-app windows through trusted virtual displays. Docs:
  `docs/windowing.md`. Instance `win`.
- audio (M2): Rust `bridge/` plus `audio.primary.linuxapk`, played to PipeWire.
  Docs: `docs/audio.md` and `docs/bridge.md`. Instance `audio`.
- ubinder (M4): userspace binder through the seccomp supervisor. Docs:
  `docs/ubinder.md`. Instance `ubinder`.
- image: boot the CI cuttlefish build (`tools/fetch-image.sh`, image selection
  in the session). Docs: `docs/image.md`. Instance `image`.
- cli: the `linuxapk` host command (install, `.desktop` files, run). Docs:
  `docs/cli.md`. Instance `cli`.
- integ (M3): bridge/system-app channel (dark mode, accent, time zone, locale),
  notifications, clipboard, opening URLs/Custom Tabs, MPRIS. Instance `integ`.
- integb (M3): IME (`text-input-v3`), document picker (FileChooser portal),
  in-Android installs mirrored to `.desktop` entries, microG, fractional scaling
  to density. Instance `integb`.
- gui (done): the GTK4/libadwaita manager app (`gui/`) on the shared `core/`
  library. Docs: `docs/gui.md`. Instance `gui`.
- compat: F-Droid corpus, automated runner, COMPATIBILITY.md, triage. Docs:
  `docs/compat-triage.md`.
- input (done, e2e 15/15): Kotlin platform-signed system app
  (`dev.linuxapk.system`) that injects the composer's Wayland input stream into
  Android. Docs: `docs/input.md`.
- image builder: owned by the orchestrator. AOSP sync of `~/aosp/a17` runs as
  systemd user unit `linuxapk-aosp-sync` (`~/aosp/sync-loop.sh`), build
  container `aosp/build-shell.sh`.
- Resilience: the host app can restart and kill agents and background shells.
  Long jobs run under `systemd-run --user`; lanes push WIP to `lane/<name>`.

## Current state

- 2026-10-26: repo created. M0 started.
- 2026-10-27 (session): **M0a reached:** `sys.boot_completed=1` rootless, with
  no AOSP patches. eBPF is emulated through seccomp user notification
  (`session/helper/bpfemu.c`), and iptables uses the nf_tables backend. The host
  Wayland socket is bound at `/dev/socket/wayland-host` (`start --headless` to
  skip it). See `docs/m0-session.md`.
- 2026-10-27: AOSP 17 synced (`~/aosp/a17`, 158 GB). First full build (paused at
  77% after the OOM incident, see above) runs as systemd unit
  `linuxapk-aosp-build`: `aosp_x86_64-cp2a-userdebug` (the tree's BUILD_ID is
  CP2A.260605.016), log `~/aosp/a17-build.log`, driven by
  `~/aosp/build-full.sh`.
- 2026-10-27 (gpu): `gpu/build.sh` NDK-builds Mesa 26.2.5 RADV and lavapipe into
  `out/vendor-overlay/lib64/hw/vulkan.{radv,lvp}.so` (LL-NDK deps only). Both
  pass compute, udmabuf import and swapchain-image import via
  `mapper.linuxapk.so` in a Bionic namespace. In the session, GLES 3.2 via ANGLE
  runs on RADV. See [gpu.md](gpu.md).
- 2026-10-27 (gpu): fixed the bootanimation crash in RADV's
  `vkAcquireNextImageKHR` (deferred-allocation swapchain images; Mesa patch
  0003). Vulkan acquire/present loops and bootanimation (ANGLE on RADV) now run
  in the session, with real release sync_files from amdgpu (no sw_sync needed).
- 2026-10-27 (gpu): video no longer aborts SurfaceFlinger. RADV reports YV12,
  NV12 and NV21 AHBs as external formats with YCbCr conversion through u_gralloc
  (Mesa patch 0005). NewPipe plays video with correct colors; compat is 30/31.
- 2026-10-27 (hal): `hal/build.sh` NDK-builds allocator AIDL V3,
  `mapper.linuxapk.so` (IMapper v5, memfd→udmabuf linear buffers) and composer3
  V5 (headless, all-CLIENT, timer vsync, PNG frame dumps) into
  `out/vendor-overlay`. Proven in a namespace with the image's servicemanager,
  incl. AHardwareBuffer via libui. See [hal-display.md](hal-display.md). In the
  session SF uses them (display 0 1280x800@60, client targets allocated); first
  composed frame still pending a layer (bootanimation crashes in RADV
  `vkAcquireNextImageKHR`).
- 2026-10-27 (hal): Wayland backend: `vendor.linuxapk.hwc.backend=wayland` shows
  display 0 as a "Linux.apk" KWin window (linux-dmabuf, wl_shm fallback) and
  streams seat input as text on `/dev/socket/linuxapk-input`. Session lane: bind
  the host socket at `/dev/socket/wayland-host` and set the property.
- 2026-10-27 (input): "Linux.apk System" (`android/`, Kotlin, platform-signed
  persistent priv-app `dev.linuxapk.system` in `out/system_ext-overlay`) turns
  the composer's input stream into injected mouse/touch/key events; its stream
  passes the live InputDispatcher's verifier. End-to-end test waits for
  `boot_completed` (`android/e2e/e2e.sh`). See [input.md](input.md).
- 2026-10-27 (compat): `compat/run.sh` (Rust) runs a 16-app F-Droid corpus in
  the session and gates on regressions: 15/16 work (only arm64-only Open Sudoku
  fails). Fixed Launcher3's crash loop (gesture nav), app networking
  (EthernetService on pasta's `eth1`, DNS) and MediaCodec (BLOB pool). See
  [compat-triage.md](compat-triage.md) and `COMPATIBILITY.md`.
- 2026-10-27 (compat): 31-app corpus with a Framework column (Flutter, React
  Native, Capacitor, Godot, Compose, MapLibre) and window types (PiP, browser,
  IME, VPN, overlay, OAuth, TV): 29/31 work on the CI image in per-app windows.
  StreetComplete's MapLibre map renders and pans/zooms. Fixed app external
  storage on non-ext4 hosts (vold quota ioctls; Godot crashed). Top new issue:
  video (YV12 buffers) aborts SurfaceFlinger on RADV/ANGLE.
- 2026-10-27 (cli): `linuxapk install|run|uninstall|list|session|setup` installs
  APKs and split bundles. It writes `.desktop` entries with Android-rendered
  icons and custom-scheme handlers, and `setup` makes double-clicking an APK
  install it. `cli/tests/integration.sh` passes. See [cli.md](cli.md).
- 2026-10-27 (ubinder): **M4 milestone 1** — the userspace binder driver state
  machine (`ubinder/`, Rust) is implemented and proven: 8 state-machine tests
  (register/transact, sync reply routing, fd passing, oneway, node ref
  handshake, death notification) plus a fuzzer (2M iterations, no panics).
  `mise run check` passes. Next: the seccomp supervisor binding, a `--ubinder`
  session flag, and full boot with corpus parity. Kernel binder stays the
  default. See [ubinder.md](ubinder.md).
- 2026-10-27 (inventory): [integration.md](integration.md) lists every Android
  API, service and input type with a desktop counterpart (141 rows: 10 done, 19
  partial, 101 todo, 11 won't-do), with owner lanes, priorities, test apps and a
  recommended order for M3.
- 2026-10-27 (win): **per-app windows (M2).** Each app runs on a trusted virtual
  display created by the system app's window manager. SurfaceFlinger hands its
  frames to the composer (HWC-backed virtual displays), and the composer shows
  each display as its own `xdg_toplevel` (`app_id` `dev.linuxapk.<pkg>`).
  Resize, close, focus and input protocol v2 work, and `linuxapk run` launches
  through `dev.linuxapk.LAUNCH`. Display 0 is headless unless
  `start --single-window`. The Wayland client now lives in the host bridge's
  display service (SSD on KDE, Adwaita CSD elsewhere, e.g. GNOME); the composer
  sends it dma-buf frames. See [windowing.md](windowing.md).
- 2026-10-27 (audio): **sound works.** The host bridge `linuxapk-bridge`
  (`bridge/`, Rust: wire/transport/registry core, the audio service, and an
  events API for the display service) plays the HAL's output to PipeWire. Our
  `libtinyalsav2.so` (`audio/`) replaces the image's tinyalsa and writes into a
  sealed memfd ring. A 440 Hz AAudio tone captures at 439.99 Hz and Mindustry is
  audible, on the emulator and CI images. Latency is ~86 ms, set by AOSP's AIDL
  HAL (85 ms nominal); our share is ~34 ms. Next: the CI APEX install point with
  the image lane, then our own AIDL stream for < 50 ms. See [audio.md](audio.md)
  and [bridge.md](bridge.md).
- 2026-10-27 (image): **the session boots the AOSP CI image by default**
  (`aosp_cf_x86_64_only_phone-userdebug` 16644040, fetched and verified by
  `tools/fetch-image.sh`; `LINUXAPK_IMAGE=emulator` for the old one). Vendor
  APEXes are selected cuttlefish's way (`androidboot.vendor.apex.*`), with the
  VM-only ones blanked. Parity: boot ~24 s from a fresh `/data` (emulator 22.5
  s), idle anon memory the same, selftest 12/12, input and audio e2e pass,
  compat 15/16 with no regressions (headless composer). See
  [image.md](image.md).

- 2026-10-27 (gui): **manager app.** `linuxapk-gui` (GTK 4/libadwaita) on
  `linuxapk-core`, split out of `cli/`, installs APKs from a confirmation page
  (drop, file chooser, or "open with"; `linuxapk setup` now registers it), and
  lists, opens and uninstalls apps, with session status and settings. Proven in
  nested KWin. See [gui.md](gui.md).

## Backlog (unassigned)

- HAL: input protocol v2 (scroll source, repeat rate/delay, keymap for non-US
  layouts), host cursor on pointer enter, zero-copy attach of SurfaceFlinger
  buffers, per-layer subsurfaces, frame-callback vsync, present/release fences
  via `DMA_BUF_IOCTL_EXPORT_SYNC_FILE`.
- logd ignores `ro.logd.size=16M` from vendor build.prop (still 256 KiB); the
  workaround is `logcat -G 16M` after boot.
- clang-tidy once host-side `compile_commands.json` exists.

## Next

- M0: extract the prebuilt image, then boot servicemanager, system_server and
  zygote in a rootless namespace.
