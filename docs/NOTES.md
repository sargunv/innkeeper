# Orchestrator notes

Working state for whoever (human or agent) picks this up. Keep this file short:
current state, decisions, next steps. Put history in git.

## Ground rules

- Push straight to `main` (no PRs). The repo is private, with no CI for now.
  Post nothing outside this repo.
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
- TODO (session lane): lock the masks against umount by entering a nested user
  and mount namespace before exec'ing init.
- TODO: give the session its own network namespace (pasta) so netd can't touch
  the host network namespace.

## Locations

- Repo: `~/Projects/linux.apk`
- AOSP checkout (android-17.0.0_r1): `~/aosp/a17` (outside the repo)
- Prebuilt image: `~/Android/Sdk/system-images/android-37.0/default/x86_64`
- Large working files: `~/Projects/linux.apk/out/` (gitignored)

## Findings (graphics)

- AOSP 17 `external/` includes mesa3d (26.1-devel; Android.bp builds only
  lavapipe, zink and gfxstream, not RADV), minigbm (intel/msm/meson variants,
  no amdgpu), drm_hwcomposer (has an hwc3 AIDL frontend), angle, swiftshader,
  wayland and wayland-protocols, all buildable for Android.
- RADV uses ACO and needs no LLVM, so it can be cross-built out of tree with
  meson and the NDK. ANGLE (already in /system) provides GLES on top of
  RADV/lavapipe Vulkan.
- Device access for this user: `/dev/dri/renderD128` (world rw), and
  `/dev/udmabuf` and `card1` (logind ACL). There's no access to
  `/dev/dma_heap/system`. So memfd → udmabuf gives CPU-backed dma-bufs for a
  simple gralloc.

## Lanes (worktrees in `~/Projects/linux.apk-wt/<lane>`, branch `lane/<lane>`, rebase then push to main)

- session: namespace launcher, APEX flattening, init boot (M0a). Docs: `docs/m0-session.md`.
- hal: allocator, mapper (stable-C v5), composer3, built with the NDK and
  installed into `out/vendor-overlay`. Docs: `docs/hal-display.md`.
- gpu: RADV plus a software Vulkan driver, cross-built with the NDK into
  `out/vendor-overlay`. Docs: `docs/gpu.md`.
- image builder: owned by the orchestrator. AOSP sync of `~/aosp/a17` runs as
  systemd user unit `linuxapk-aosp-sync` (`~/aosp/sync-loop.sh`), build container
  `aosp/build-shell.sh`.
- Resilience: the host app can restart and kill agents and background shells.
  Long jobs run under `systemd-run --user`; lanes push WIP to `lane/<name>`.

## Current state

- 2026-10-26: repo created. M0 started.

## Next

- M0: extract the prebuilt image, then boot servicemanager, system_server and
  zygote in a rootless namespace.
