# GitHub Actions

`CI` runs `mise run check` in parallel with `build`, `test` and `test:e2e` on
every push and pull request. The checks build uploads a `stage` artifact
containing the release stage tarball and its `manifest.tsv` (path, owning module
and SHA-256). The tarball preserves executable modes and symlinks. The scheduled
run (08:23 UTC daily) and `workflow_dispatch` run `package:flatpak`, then the
full `test:compat` corpus twice: headless and windowed. Each mode keeps its
report and session logs in the `compatibility` artifact, including on failure.
Both compare against the committed baselines; the wrapper restores the harness's
tracked output files after each run.

The nightly renders both modes' unmerged `fresh-results.json` to the
[Compatibility wiki](https://github.com/sargunv/linux.apk/wiki/Compatibility),
including app versions, frameworks, native code, results, notes and the image
build. Missing mode results are marked explicitly. Run metadata, timings, memory
and budget measurements stay in the logs and artifacts so repeated runs with
unchanged compatibility produce the same page. The rendered page is also saved
in the artifact. Wiki errors are warnings and never replace the corpus exit
status; unchanged pages are not committed. Publishing uses the repository
Actions secret `LINUXAPK_WIKI_TOKEN` described in
[tests/README.md](../../tests/README.md). Without it, publishing is skipped with
a notice. The job's `GITHUB_TOKEN` keeps only `contents: read`. A newly enabled
wiki needs an initial page created by the owner before its Git remote exists.

Test and render without running any apps (the fixture includes a harness error):

```sh
python3 -B -m unittest discover -s tools/ci/tests
mkdir -p out/wiki-fixture/headless
cp tools/ci/tests/fixtures/results.json out/wiki-fixture/headless/fresh-results.json
python3 tools/ci/compat-wiki.py --reports out/wiki-fixture \
  --output out/wiki-fixture/Compatibility.md
```

```sh
gh workflow run ci.yml --ref <branch>
gh run watch <run-id> --exit-status
```

Ubuntu 26.04 supplies the project's GTK baseline, KWin and Flatpak. Only
`setup.sh`, guarded by `GITHUB_ACTIONS=true`, uses sudo. It enables unprivileged
user namespaces, starts the systemd user manager and runner PipeWire server, and
grants runner-local read/write access to udmabuf (built into the runner's Azure
kernel). It also sets Yama `ptrace_scope=0` on the disposable runner: Ubuntu's
default blocks the sibling broker's process-memory access and prevents init from
creating its property area. #52 tracks the supervisor ancestry change needed to
run under that default without host configuration. A missing udmabuf module
fails setup: software Vulkan still needs real dma-buf allocations, so a fake
device or skipping the runtime tests would not test Android graphics.

The hosted GPU-less VM also needs the in-kernel `vgem` DRM device for KWin's
virtual OpenGL backend. Setup installs the matching kernel's extra modules if
vgem is absent, loads it, and grants the runner access to its card node. This
provides buffer allocation for the software compositor; Android still uses
lavapipe and udmabuf. This provisioning is confined to disposable hosted
runners. Contributors use their existing DRM device without host setup.

The test tasks run the stage through the repository's Flatpak development
driver. `test:e2e` starts its own isolated desktop; both compatibility modes use
`tests/nested.sh`. That runner provides virtual KWin, a private bus, runtime
directory and HOME/XDG directories, and stops the desktop when done. The
runner's user manager supplies software-rendering settings to KWin. Android
frames require its OpenGL compositor, even with software rendering: QPainter
does not expose the linux-dmabuf protocol used by the bridge. Android sessions
are named and memory capped by the development driver. The nightly also builds
and installs the distributable Flatpak. CI sets `LINUXAPK_MIN_AVAILABLE_GIB=10`
for the 16 GB runner; the driver's default remains 25 GiB for local desktops,
and its 8 GiB hard cap remains.

An exact stage cache skips building when none of its source, build script or
toolchain inputs changed; the probes APK is restored with the stage. Tests still
run on every push and PR. Gradle outputs and dependency caches restore on their
source/toolchain hash, with incremental reuse after source changes.

Mise caches its pinned tools. Separate caches cover the SDK pin, Cargo and
Gradle inputs, the Mesa/LLVM and image-tool install trees on separate input
hashes, the Android image on its pin/extraction inputs, and the APK corpus on
its checksums. Each expensive module is saved immediately after it builds,
before the rest of the build or tests can fail. The first build is substantially
slower than a warm run; the 120-minute check timeout allows a cold LLVM build.
No Android image or third-party APK is uploaded as a workflow artifact.
