# Linux.apk

Run Android apps on desktop Linux: a real AOSP 17 userspace in a rootless
namespace session, with our own vendor HALs and a desktop bridge. The plan is in
[docs/STRATEGY.md](docs/STRATEGY.md), and the current state is in
[docs/NOTES.md](docs/NOTES.md).

## Project map

```
aosp/        AOSP build container (aosp/build-shell.sh); tree lives in ~/aosp/a17
docs/        STRATEGY (plan), NOTES (live state), per-lane design docs
gpu/         RADV + lavapipe Vulkan drivers cross-built with the NDK, Mesa patches
hal/         graphics allocator, IMapper v5, composer3 (C++, NDK); AOSP AIDL copies
selinux/     dev SELinux module (binder for unconfined_t)
session/     namespace launcher, ns-init, pivot-exec, preload shim, rootfs overlay
tools/       host-tools container, image extraction, APEX flattening
out/         (gitignored) extracted image, flattened APEXes, vendor-overlay, builds
```

## Dev tool commands

- `mise install`: tools (hk, dprint, clang-format, shellcheck, shfmt, Rust).
- `mise run check` / `mise run fix`: lint and format everything (hk + dprint).
  The pre-commit hook runs hk on staged files.
- `tools/extract-image.sh <sdk-image-dir> out/img`, then
  `tools/flatten-apex.sh`: prepare the prebuilt Android image.
- `hal/build.sh`, `gpu/build.sh`: build the HALs and drivers into
  `out/vendor-overlay` (each runs its tests).
- `session/linuxapk-session start|stop|status|log|exec|shell|selftest`: run the
  Android session.

## Project invariants

- **Host safety.** Inside our user namespace, Android "root" is the host user.
  Every namespace running Android code must mask and lock host sysfs submounts,
  get its own systemd unit/cgroup and network namespace, and must not bind host
  `/run/user`, `$HOME` or `/dev` wholesale. `session/linuxapk-session selftest`
  must pass. Never use sudo.
- **No root at runtime.** Never patch the prebuilt image in place. Prefer
  Android's stable boundaries (HAL AIDL, NDK APIs, overlays) over AOSP patches.
  Any AOSP patch needs a written reason.
- **Vendor binaries link only LL-NDK system libraries** plus what we ship in
  `/vendor/lib64`. Build them out of tree with the NDK.
- **Only the image-builder lane runs AOSP builds** in `~/aosp/a17`.
- **Languages:** C/C++ for code loaded into Android processes or on C ABIs; Rust
  for new standalone components (see STRATEGY).
- **Workflow:** push straight to `main` (rebase, don't merge), and keep the repo
  private. Generated files go in `out/`. Long jobs run under
  `systemd-run --user`.
