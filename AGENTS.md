# Linux.apk

Run Android apps on desktop Linux: a real AOSP 17 userspace in a rootless
namespace session, with our own vendor HALs and a desktop bridge. The plan is in
[docs/STRATEGY.md](docs/STRATEGY.md), and the current state is in
[docs/NOTES.md](docs/NOTES.md).

## Project map

```
host/        runs on the desktop: core library, linuxapk CLI, manager app,
             desktop bridge, session launcher (linuxapk-session, ns-init.sh)
supervisor/  the syscall boundary: pivot-exec, the binder broker, the shim
image/       the pinned AOSP CI image: pin, bootconfig, fetch/extract/flatten
android/     runs inside Android: vendor HALs and drivers (graphics, vulkan,
             audio), system pieces (exec relay, iptables), the system app,
             the config overlay, microG
tests/       cross-module tests: bionic harness, e2e suites, compat corpus
tools/       build and dev tooling: build/ (toolchain pin, artifacts, stage),
             container/ (the build container), dev/window-shot.sh
docs/        STRATEGY (plan), NOTES (live state), per-lane design docs
out/         (gitignored) build output only: cargo/, build/<module>/,
             stage/ (the installable prefix), test/
```

## Dev tool commands

- `mise install`: tools (hk, dprint, shellcheck, shfmt, Rust, Java, CMake,
  Ninja). `tools/build/toolchain.env` pins the NDK, API and build-tools.
- `mise run check` / `mise run fix`: lint and format everything (hk + dprint).
  The pre-commit hook runs hk on staged files.
- `mise run build`: every `build:<module>` (each fills
  `out/build/<module>/install`), then `stage` merges them into `out/stage`, the
  installable prefix, checked against `tools/build/artifacts.toml`.
  `mise run clean` removes build output. Nothing at runtime builds anything.
- `mise run session -- start|stop|status|log|exec|shell|selftest|prepare`: the
  staged launcher (`out/stage/libexec/linuxapk/linuxapk-session`). It keeps
  runtime files in `$XDG_RUNTIME_DIR/linuxapk[-<instance>]` and logs in
  `$XDG_STATE_HOME/linuxapk[-<instance>]`, never in the checkout.
- `mise run test` (unit), `test:bionic`, `test:binder`, `test:boot`, `test:e2e`,
  `test:compat`: the test tiers; the integration ones need `mise run build` and
  a prepared image.

## Project invariants

- **The user runs no main session (2026-10-27).** Don't start, restore or
  reinstall one, and don't install host integration (`~/.local/bin`, desktop
  entries, mime handlers) unless the user asks. Tests use named instances.

- **Memory safety for the host** (incident 2026-10-27: a global OOM crashed the
  user's desktop). `/tmp` is RAM-backed tmpfs on the dev machine, so never write
  large files (logcat dumps, downloads, build trees) there; use `out/`. Every
  long job runs under a memory cap:
  - the session unit is capped at `MemoryHigh=6G` / `MemoryMax=8G` (an idle
    session uses about 1.3 GB);
  - podman builds pass `--memory=`, and the AOSP build is capped at 32 GB with
    `-j12`. Don't run the AOSP build alongside other heavy work.
  - Never `logcat -b all` or an unbounded `logcat` into a file; bound it with
    `-d`, `-t <n>` or `-s <tags>`.

- **Host safety.** Inside our user namespace, Android "root" is the host user.
  Every namespace running Android code must mask and lock host sysfs submounts,
  get its own systemd unit/cgroup and network namespace, and must not bind host
  `/run/user`, `$HOME` or `/dev` wholesale. `linuxapk-session selftest` must
  pass (`mise run test:boot`). Never use sudo.
- **Nested test desktops are fully isolated** (incident 2026-10-27: nested
  portals opened dialogs on the user's real desktop, and a nested plasmashell
  started kded, kdeconnect and akonadi against the user's real config and data).
  Every nested compositor test gets a private `dbus-run-session`, a private
  `XDG_RUNTIME_DIR`, `WAYLAND_DISPLAY` set to the nested socket, `DISPLAY`
  unset, and temporary `XDG_CONFIG_HOME`/`XDG_DATA_HOME`/`XDG_CACHE_HOME` under
  `out/`. Never start plasmashell, kded, akonadi or kdeconnect against the
  user's real dirs, and kill everything you started when you're done.
- **No root at runtime.** Never patch the prebuilt image in place. Prefer
  Android's stable boundaries (HAL AIDL, NDK APIs, overlays) over AOSP patches.
  Any AOSP patch needs a written reason.
- **Vendor binaries link only LL-NDK system libraries** plus what we ship in
  `/vendor/lib64`. Build them out of tree with the NDK.
- **Only the image-builder lane runs AOSP builds** in `~/aosp/a17`.
- **Languages:** C/C++ for code loaded into Android processes or on C ABIs; Rust
  for new standalone components (see STRATEGY).
- **Workflow:** push straight to `main` (rebase, don't merge). The repo is
  public: never commit secrets, image contents or third-party APKs. Never commit
  media files: upload screenshots with
  `gh issue comment 1 --attach <file>#<alt>` (the README media issue) and link
  the user-attachments URL. `tools/dev/window-shot.sh` captures only the
  Linux.apk window. Generated files go in `out/`. Long jobs run under
  `systemd-run --user`.
