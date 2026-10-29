# tools

Build and dev tooling; never shipped.

- **`build/`**: `toolchain.env` (the one NDK, API, build-tools and compileSdk
  pin, loaded by mise, the container and Gradle), `artifacts.toml` (each
  module's build task, where its artifacts run, and the artifacts the stage
  requires), `stage.sh` (merges `out/build/*/install` into `out/stage` and
  writes `share/linuxapk/manifest.tsv`), `host.sh` (`build:host`) and
  `install-component.sh` (installs one module of the NDK superbuild).
- **`container/`**: the one Fedora build container (`Containerfile`) and
  `run.sh`, which tags the image by the Containerfile's hash, mounts `$HOME` for
  the NDK, and always caps the run (`--memory`, `--pids-limit`, at most 12
  jobs). Used for Mesa and the static image tools.
- **`dev/session.sh`, `dev/linuxapk.sh`** (both `dev/run.py`): the staged
  launcher and CLI in a private Flatpak, capped from the host
  ([packaging/flatpak](../packaging/flatpak/README.md#development));
  **`dev/benchmark.sh`**: boot, idle memory, Binder throughput and frame timing,
  for comparing the two.
- **`dev/footprint.sh`**: PSS and RSS of every process in a running session, and
  its cgroup's memory, to see what an idle session holds.
- **`dev/window-shot.sh`**: a screenshot of just a Linux.apk window on KDE, for
  bug reports and the README media issue.

How these fit together: [docs/development.md](../docs/development.md).
