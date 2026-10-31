# tools

Build and dev tooling; never shipped.

- **`build/`**: `toolchain.env` (the one NDK, API, build-tools and compileSdk
  pin, loaded by mise, the container and Gradle), `stage.sh` (copies
  `out/build/*/install` into `out/stage` and writes
  `share/innkeeper/manifest.tsv`), `host.sh` (`build:host`) and `sccache.sh`
  (runs a build against the compiler cache every checkout shares).
- **`container/`**: the one Fedora build container (`Containerfile`) and
  `run.sh`, which tags the image by the Containerfile's hash, mounts `$HOME` for
  the NDK and the compiler cache, and always caps the run (`--memory`,
  `--pids-limit`, at most 12 jobs) at nice 10. Used for Mesa and the static
  image tools.
- **`dev/session.sh`, `dev/innkeeper.sh`** (both `dev/run.py`): the staged
  launcher and CLI in a private Flatpak, capped from the host
  ([packaging/flatpak](../packaging/flatpak/README.md#development)).
- **`dev/footprint.sh`**: PSS and RSS of every process in a running session, and
  its cgroup's memory, to see what an idle session holds.
- **`dev/window-shot.sh`**: a screenshot of just an Innkeeper window on KDE, for
  PRs, bug reports and the README media issue.

How these fit together: [docs/development.md](../docs/development.md).
