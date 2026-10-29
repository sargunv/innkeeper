# GitHub Actions

`CI` runs `mise run check`, `build`, `test` and `test:e2e` on every push and
pull request. The scheduled run (08:23 UTC daily) and `workflow_dispatch` run
`package:flatpak`, then the full `test:compat` corpus twice: headless and
windowed. Each mode keeps its report and session logs in the `compatibility`
artifact, including on failure. Both compare against the committed baselines;
the wrapper restores the harness's tracked output files after each run.

The nightly renders both modes' saved JSON to the
[Compatibility wiki](https://github.com/sargunv/linux.apk/wiki/Compatibility),
including app versions, frameworks, native code, results, notes and the image
build. Missing mode results are marked explicitly. Run metadata, timings, memory
and budget measurements stay in the logs and artifacts so repeated runs with
unchanged compatibility produce the same page. The rendered page is also saved
in the artifact. Wiki errors are warnings and never replace the corpus exit
status; unchanged pages are not committed. The report uses `GITHUB_TOKEN` with
`contents: write` only in the nightly job. No extra credential is configured
automatically. If GitHub rejects wiki access, the repository owner must supply a
repository Actions secret named `LINUXAPK_WIKI_TOKEN` containing a **classic PAT
with `public_repo` scope** from an account with write access to this public
repository, then switch the publishing step's `GH_TOKEN` to that secret. A newly
enabled wiki needs an initial page created by the owner before its Git remote
exists.

Test and render without running any apps (the checked-in baseline is a saved
results file):

```sh
python3 -B -m unittest discover -s tools/ci/tests
mkdir -p out/wiki-fixture/headless
cp tests/compat/results.json out/wiki-fixture/headless/results.json
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
kernel). A missing udmabuf module fails setup: software Vulkan still needs real
dma-buf allocations, so a fake device or skipping the runtime tests would not
test Android graphics.

The test tasks run the stage through the repository's Flatpak development
driver. `test:e2e` starts its own isolated desktop; both compatibility modes use
`tests/nested.sh`. That runner provides virtual KWin, a private bus, runtime
directory and HOME/XDG directories, and stops the desktop when done. The
runner's user manager supplies software-rendering settings to KWin. Android
sessions are named and memory capped by the development driver. The nightly also
builds and installs the distributable Flatpak. CI sets
`LINUXAPK_MIN_AVAILABLE_GIB=10` for the 16 GB runner; the driver's default
remains 25 GiB for local desktops, and its 8 GiB hard cap remains.

Mise caches its pinned tools. Separate caches cover the SDK pin, Cargo and
Gradle inputs, the Mesa/LLVM and image-tool install trees on their source
hashes, the Android image on its pin/extraction inputs, and the APK corpus on
its checksums. The first build is substantially slower than a warm run; the
60-minute check timeout allows a cold LLVM build. No Android image or
third-party APK is uploaded as a workflow artifact.
