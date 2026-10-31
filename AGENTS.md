# Innkeeper

Run Android and OpenHarmony apps on Linux: a real AOSP 17 userspace in a Flatpak
session, with our own vendor HALs, a seccomp supervisor and a desktop bridge.
Start with [docs/architecture.md](docs/architecture.md); its module table says
what each directory is and where it runs.

## Commands

- `mise install`: toolchains ([toolchain.env](tools/build/toolchain.env) pins
  the Android ones) and hk's pre-commit hook, which fixes staged files; fix what
  it reports, never bypass it.
- `mise run check` / `fix`, `build` (into `out/stage`), `clean`,
  `session -- start|stop|status|log|exec|shell|selftest|prepare`.
- `mise run test`, `test:e2e -- [group]`, `test:bionic`,
  `test:compat -- --only <package>` ([tests/README.md](tests/README.md)).

## Hard constraints

- **Never sudo**, nor anything that needs root, a system package, an SELinux or
  AppArmor profile or a kernel module; build dependencies go in the build
  container or mise. Only `tools/ci/setup.sh` uses sudo, on GitHub-hosted
  runners.
- **Memory.** `/tmp` is RAM: large files go in `out/`. Run long jobs under
  `systemd-run --user -p MemoryMax=…`, and check `free -g` before a session.
- **Bounded logcat** only (`-d`, `-t <n>`, `-s <tags>`), never `-b all`.
- **Host safety.** The Flatpak sandbox is the boundary
  ([security.md](docs/security.md#the-sandbox)); `selftest` must pass
  (`mise run test:e2e -- boot`).
- **Throwaway profiles.** Run anything with a fresh `INNKEEPER_PROFILE=<name>`
  and stop it after. Never touch `default` or the user's own profiles, or
  install host integration (`mise run package -- --install`), unless asked. The
  dev runner makes a test session unless it runs on a nested desktop.
- **Isolated desktops and buses.** Anything that opens windows, portals or
  notifications runs under `tests/nested.sh`. Never pair a private D-Bus with
  the real `XDG_RUNTIME_DIR`, never start plasmashell, kded, Akonadi or KDE
  Connect on the user's dirs, and never restart the user's portals or buses.
- **Desktop baseline:** current major distro releases
  ([architecture.md](docs/architecture.md#packaging-scope)).
- **Stable boundaries.** Never patch the image; use HAL AIDL, NDK APIs, rc and
  overlay files, and give any AOSP patch a written reason. Vendor binaries link
  only LL-NDK libraries plus what we ship in `/vendor/lib64`, built with the
  NDK.
- **Languages:** C/C++ in Android processes or on C ABIs, Rust for standalone
  components, Kotlin for the system app.
- **Threat model: desktop parity** ([security.md](docs/security.md)): simple
  bounds and correctness checks, no guards against malicious Android processes.

## Tests

Test observable behaviour we own: unit tests take branches, edge cases and
errors; e2e keeps one common-path scenario per user-visible feature and waits
for observations, never fixed delays. A bug fix comes with a test that fails
without it: no reproduction, no fix.

## Workflow

- One PR per change, logical commits, `mise run check` before every push; never
  force-push `main`.
- The repo is public: no secrets, image contents or third-party APKs; generated
  files go in `out/`; post nothing outside this repo.
- Never commit media: attach screenshots to the PR
  (`gh issue comment <pr> --attach <file>#<alt>`) and link them from its
  description.
- Plans and open work are GitHub issues. Docs say what is true now and why; code
  comments say what the code can't.
