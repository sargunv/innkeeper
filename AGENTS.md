# Linux.apk

Android apps on desktop Linux: a real AOSP 17 userspace in a rootless namespace
session, with our own vendor HALs, a seccomp supervisor and a desktop bridge.
Start with [README.md](README.md) (the module table says what each dir is and
where it runs), then [docs/architecture.md](docs/architecture.md).

## Commands

- `mise install`: toolchains and the pre-commit hook.
  `tools/build/toolchain.env` pins the NDK, API and build-tools.
- `mise run check` / `mise run fix`: lint and format everything (hk, dprint).
  The pre-commit hook runs hk on staged files; fix lints, never bypass it.
- `mise run build`: every `build:<module>`, then `stage` into `out/stage`.
  `mise run clean` removes build output.
- `mise run session -- start|stop|status|log|exec|shell|selftest|prepare`.
- `mise run test` (unit), `test:bionic`, `test:binder`, `test:boot`, `test:e2e`,
  `test:compat`; see [tests/README.md](tests/README.md).

## Rules

- **Never sudo.** Nothing the project needs, to build or to run, may require
  root, a system package, an SELinux module, an AppArmor profile or a kernel
  module. Build dependencies go in the build container or mise.
- **Memory caps** (a global OOM once crashed the user's desktop). `/tmp` is a
  RAM-backed tmpfs: never write large files there; use `out/`. Sessions run
  capped (`MemoryHigh=6G`, `MemoryMax=8G`), container builds always pass
  `--memory` and `--pids-limit` (`tools/container/run.sh`), and long jobs run
  under `systemd-run --user` with a `MemoryMax`. Check `free -g` before starting
  another session.
- **Never `logcat -b all`** or an unbounded `logcat` into a file; bound it with
  `-d`, `-t <n>` or `-s <tags>`.
- **Host safety.** Inside the session Android's root is the host user. Every
  namespace running Android code masks and locks host sysfs, has its own network
  namespace and cgroup, and never binds host `/run/user`, `$HOME` or `/dev`
  wholesale. `linuxapk-session selftest` must pass (`mise run test:boot`).
- **Test sessions are named.** Use `LINUXAPK_INSTANCE=<name>` with a fresh data
  dir for anything you run, and stop it when done. Don't start, restart or reset
  the user's main session (no instance name), and don't install host integration
  (`mise run install-dev`, desktop entries, MIME handlers) unless the user asks.
- **Nested test desktops are fully isolated** (a nested plasmashell once started
  kded, KDE Connect and Akonadi against the user's real config, and nested
  portals opened dialogs on the real desktop): a private `dbus-run-session`, a
  private `XDG_RUNTIME_DIR`, `WAYLAND_DISPLAY` set to the nested socket before
  the bus starts, `DISPLAY` unset, and
  `HOME`/`XDG_{CONFIG,DATA,CACHE,STATE}_HOME` under `out/`. Never start
  plasmashell, kded, Akonadi or KDE Connect against the user's real dirs, and
  kill everything you started. Recipe:
  [docs/development.md](docs/development.md#a-nested-isolated-desktop).
- **Stable boundaries only.** Never patch the image in place; prefer HAL AIDL,
  NDK APIs, rc and overlay files. Any AOSP patch needs a written reason.
- **Vendor binaries link only LL-NDK system libraries** plus what we ship in
  `/vendor/lib64`, built out of tree with the NDK.
- **Languages:** C/C++ for code loaded into Android processes or on C ABIs; Rust
  for standalone components; Kotlin for the system app.
- **Threat model: desktop parity** ([docs/security.md](docs/security.md)). Keep
  simple bounds and correctness checks; don't add guards that only defend
  against deliberately malicious Android processes.

## Workflow

- Push straight to `main`: rebase, never merge, never force-push. Logical
  commits with clear messages; `mise run check` passes before every push.
- The repo is public: never commit secrets, image contents or third-party APKs.
  Generated files go in `out/`. Post nothing outside this repo.
- Never commit media. Upload screenshots to the README media issue with
  `gh issue comment 1 --attach <file>#<alt>` and link the user-attachments URL;
  `tools/dev/window-shot.sh` captures just the Linux.apk window.
- Plans and open work are GitHub issues, not docs. Docs say what is true now and
  why; code comments say what the code can't.
