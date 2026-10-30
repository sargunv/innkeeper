# GitHub Actions

`CI` runs on pull requests and pushes to main, avoiding duplicate feature-branch
push and PR jobs and their large, separately scoped cache copies. It classifies
changes, then runs lint and a single release build. Unit tests and e2e run in
parallel on separate runners, using that build's `stage` artifact (release
tarball, test probes and `manifest.tsv`). Tarballs preserve executable modes and
symlinks. The `checks` gate requires every applicable job to succeed; a failed
or cancelled dependency cannot turn into a green skipped gate.

Changes only to README files, `AGENTS.md`, Markdown under `docs/`, or `LICENSE`
run formatting and script/workflow lint without Clippy, build or runtime tests.
Unknown paths, missing history and new branches run all checks. PRs compare the
base and merge commit; pushes compare the previous and current commit.

The scheduled run (08:23 UTC daily) and `workflow_dispatch` run
`package:flatpak`, then the full `test:compat` corpus twice: headless and
windowed. Each mode keeps its report and session logs in the `compatibility`
artifact, including on failure. Both compare against the committed baselines;
the wrapper restores tracked output files after each run.

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

Inspect a push or PR run with `gh run view <run-id>`. Do not dispatch the
workflow to test CI changes: dispatch runs the nightly corpus.

To measure incremental compilation, rerun a PR run at its pinned commit after
removing only its exact stage cache. Keep dependency and toolchain caches. New
PR events can include a newer merge of main and changed dependency pins, so they
may legitimately be cold. A rerun replaces stage and diagnostic artifacts.

Ubuntu 26.04 supplies the project's GTK baseline, Mutter and Flatpak. Only
`setup.sh`, guarded by `GITHUB_ACTIONS=true`, uses sudo. It enables unprivileged
user namespaces, starts the systemd user manager and runner PipeWire server, and
grants runner-local read/write access to udmabuf (built into the runner's Azure
kernel). A missing udmabuf module fails setup: software Vulkan still needs real
dma-buf allocations, so a fake device or skipping the runtime tests would not
test Android graphics.

The GPU-less hosted VM runs headless GNOME Shell/Mutter with surfaceless
llvmpipe. Unlike KWin's OpenGL virtual backend, this needs no DRM node and
advertises linux-dmabuf v3; Android still allocates real buffers with udmabuf.
The invocation follows
[GTK's headless CI runner](https://github.com/GNOME/gtk/blob/main/.gitlab-ci/run-tests.sh)
and
[GNOME Shell's automation tests](https://github.com/GNOME/gnome-shell/blob/main/tests/meson.build).

The test tasks run the stage through the repository's Flatpak development
driver. `test:e2e` starts its own isolated desktop; both compatibility modes use
`tests/nested.sh`. That runner provides a private bus, runtime directory and
HOME/XDG directories, and stops the desktop when done. CI selects Mutter with
`LINUXAPK_TEST_COMPOSITOR=mutter`; local runs default to virtual KWin. The
runner forwards explicit software-rendering settings only to its desktop.
Android sessions are named and memory capped by the development driver. The
nightly also builds and installs the distributable Flatpak. CI sets
`LINUXAPK_MIN_AVAILABLE_GIB=10` for the 16 GB runner; the driver's default
remains 25 GiB for local desktops, and its 8 GiB hard cap remains.

An exact stage cache skips building when none of its source, build script,
workflow or toolchain inputs changed; it restores the probes APK with the stage.
Its Git tree digest includes paths, modes, symlinks and contents. An exact hit
also skips the build runner's package, SDK and compiler-cache setup. Tests still
run for code changes even when the stage is restored.

Build, unit and lint jobs cache Cargo's target directory and registry by
OS/architecture, job mode, dependency/toolchain pins and sources. A prefix
restore reuses dependency compilation after source changes; Cargo still checks
all inputs. C/C++ uses ccache, whose compiler/flags/input checks decide reuse.
Gradle's build cache and outputs use source/toolchain keys and incremental
restores; Gradle always checks its tasks. The native Mesa/LLVM and image-tool
caches retain their independent input hashes and immediate saves.

Build and unit caches save at the end of their own successful job, before or
independently of e2e. SDK caches save immediately after provisioning, and image
and private Flatpak caches save immediately after preparation, so a runtime
failure cannot discard them. Mise caches pinned tools. E2e installs runtime
packages only and restores no compiler, Gradle or Android SDK cache. Other jobs
install only their host build dependencies; docs install no system packages.

The prepared Android image hashes its pin, preparation scripts, image tools,
container and launcher preparation code; documentation and tests do not
invalidate it. Cache archives omit xattrs, so preparation always regenerates the
small single-user CLAT layer that carries Android ownership xattrs; partitions
and flattened APEXes stay cached. The private Flatpak cache hashes the
development driver, bootstrap script and Flatpak manifest; its registered
development deployment always runs the current artifact through
`--app-path=out/stage`, with permissions read from the current manifest. Neither
Android session data nor a running instance is cached. No Android image or
third-party APK is uploaded as a workflow artifact. Cold Mesa/LLVM builds still
have a 120-minute build timeout.
