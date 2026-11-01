# Innkeeper

Innkeeper runs Android and OpenHarmony apps on Linux. It runs an AOSP 17
userspace in a Flatpak session, with its own vendor HALs, a seccomp supervisor
and a desktop bridge. Read [docs/architecture.md](docs/architecture.md) first.
Its module table lists what each directory contains and where it runs.

## Commands

- `mise install` installs the toolchains and hk's pre-commit hook.
  [toolchain.env](tools/build/toolchain.env) pins the Android toolchains. The
  hook fixes staged files. Fix what it reports, and never bypass it.
- `mise run check` and `mise run fix` lint and format. `mise run build` builds
  into `out/stage`, and `mise run clean` removes build output.
  `mise run session -- start|stop|status|log|exec|shell|selftest|prepare`
  controls a development session.
- `mise run test`, `test:e2e -- [group]`, `test:bionic` and
  `test:compat -- --only <package>` run the tests
  ([tests/README.md](tests/README.md)).

## Hard constraints

- **Never sudo.** Don't use anything that needs root, a system package, an
  SELinux or AppArmor profile, or a kernel module. Build dependencies go in the
  build container or mise. Only `tools/ci/setup.sh` uses sudo, and it runs on
  GitHub-hosted runners.
- **Memory.** `/tmp` is in RAM, so put large files in `out/`. Run long jobs
  under `systemd-run --user -p MemoryMax=…`. Check `free -g` before you start a
  session.
- **Bounded logcat.** Always bound `logcat` with `-d`, `-t <n>` or `-s <tags>`.
  Never use `-b all`.
- **Host safety.** The Flatpak sandbox is the security boundary
  ([security.md](docs/security.md#the-sandbox)). `selftest` must pass
  (`mise run test:e2e -- boot`).
- **Throwaway profiles.** Run anything with a fresh `INNKEEPER_PROFILE=<name>`,
  and stop it when you finish. Never touch `default` or the user's own profiles,
  and never install host integration (`mise run package -- --install`), unless
  the user asks. Off a nested desktop, the dev runner starts a test session.
- **Isolated desktops and buses.** Run anything that opens windows, portals or
  notifications under `tests/nested.sh`. Never pair a private D-Bus with the
  real `XDG_RUNTIME_DIR`. Never start plasmashell, kded, Akonadi or KDE Connect
  on the user's directories. Never restart the user's portals or buses.
- **Desktop baseline.** Support current major distro releases
  ([architecture.md](docs/architecture.md#packaging-scope)).
- **Stable boundaries.** Never patch the image. Use HAL AIDL, NDK APIs, rc files
  and overlay files. Any AOSP patch needs a written reason. Vendor binaries link
  LL-NDK libraries and the libraries we ship in `/vendor/lib64`, and nothing
  else. They build with the NDK.
- **Languages.** Use C or C++ for code in Android processes or on C ABIs, Rust
  for standalone components, and Kotlin for the system app.
- **Threat model: desktop parity** ([security.md](docs/security.md)). Write
  simple bounds and correctness checks. Don't add guards against malicious
  Android processes.

## Tests

Test observable behaviour that we own. Unit tests cover branches, edge cases and
errors. E2e tests keep one common-path scenario per user-visible feature. They
wait for observations and never use fixed delays. A bug fix comes with a test
that fails without the fix. Without a reproduction, don't make the fix.

## Workflow

- Make one PR per change, with logical commits. Run `mise run check` before
  every push. Never force-push `main`.
- The repo is public. Never commit secrets, image contents or third-party APKs.
  Put generated files in `out/`. Post nothing outside this repo.
- Never commit media. Attach screenshots to the PR with
  `gh issue comment <pr> --attach <file>#<alt>`, and link them from the PR
  description.
- Plans and open work are GitHub issues. Docs describe what is true now and why.
  Code comments say what the code can't.
