# GitHub Actions

`CI` runs `mise run check`, `build`, `test` and `test:e2e` on every push and
pull request. The scheduled run (08:23 UTC daily) and `workflow_dispatch` run
`package:flatpak`, then the full `test:compat` corpus twice: headless and
windowed. Each mode keeps its report and session logs in the `compatibility`
artifact, including on failure. The regression and performance gates use the
repository's existing baselines; CI does not accept new baselines.

```sh
gh workflow run ci.yml --ref <branch>
gh run watch <run-id> --exit-status
```

Ubuntu 26.04 supplies the project's GTK baseline, KWin and Flatpak. Only
`setup.sh`, guarded by `GITHUB_ACTIONS=true`, uses sudo. It enables unprivileged
user namespaces, starts the systemd user manager and loads udmabuf with
runner-local read/write permissions. A missing udmabuf module fails setup:
software Vulkan still needs real dma-buf allocations, so a fake device or
skipping the runtime tests would not test Android graphics.

`desktop.sh` starts virtual KWin with software rendering, PipeWire and a private
session bus in a memory-capped scope. HOME and the XDG directories are private
under `out/ci/desktop`; DISPLAY is unset. Only the user manager's control socket
is accessible from the private runtime directory so the staged launcher can
manage its own capped service. A private `systemd-run` wrapper forwards the
desktop environment to that service without changing the user manager's
environment. Cleanup stops the named Android instance and the desktop scope. No
host integration is installed.

Mise caches its pinned tools. Separate caches cover the SDK pin, Cargo and
Gradle inputs, the Mesa/LLVM and image-tool install trees on their source
hashes, the Android image on its pin/extraction inputs, and the APK corpus on
its checksums. The first build is substantially slower than a warm run; the
60-minute check timeout allows a cold LLVM build. No Android image or
third-party APK is uploaded as a workflow artifact.

The runtime tests currently use the staged launcher, as the repository's mise
tasks do. The nightly also builds and installs the Flatpak. Changing the test
tasks to use that installation does not require copying their logic into the
workflow.
