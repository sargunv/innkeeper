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

## Locations

- Repo: `~/Projects/linux.apk`
- AOSP checkout (android-17.0.0_r1): `~/aosp/a17` (outside the repo)
- Prebuilt image: `~/Android/Sdk/system-images/android-37.0/default/x86_64`
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
  - The local AOSP build keeps running (the user's call); delete it once the
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
- win (M2): per-app windows through trusted virtual displays. Docs:
  `docs/windowing.md`. Instance `win`.
- audio (M2): Rust `bridge/` plus `audio.primary.linuxapk`, played to PipeWire.
  Docs: `docs/audio.md` and `docs/bridge.md`. Instance `audio`.
- ubinder (M4): userspace binder through the seccomp supervisor. Docs:
  `docs/ubinder.md`. Instance `ubinder`.
- image: boot the CI cuttlefish build (`tools/fetch-image.sh`, image selection
  in the session). Docs: `docs/image.md`. Instance `image`.
- cli: the `linuxapk` host command (install, `.desktop` files, run). Docs:
  `docs/cli.md`. Instance `cli`.
- compat: F-Droid corpus, automated runner, COMPATIBILITY.md, triage. Docs:
  `docs/compat-triage.md`.
- cli (M3): host command `linuxapk` (`cli/`, Rust): install, run, `.desktop`
  entries, APK open-with handler. Docs: `docs/cli.md`. Instance `cli`.
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
- 2026-10-27 (cli): `linuxapk install|run|uninstall|list|session|setup` installs
  APKs and split bundles. It writes `.desktop` entries with Android-rendered
  icons and custom-scheme handlers, and `setup` makes double-clicking an APK
  install it. `cli/tests/integration.sh` passes. See [cli.md](cli.md).

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
