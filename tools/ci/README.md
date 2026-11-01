# GitHub Actions

The `CI` workflow runs on pull requests and on pushes to main. A lint job and a
release build run first. The unit tests, e2e and OpenHarmony's boot
(`test:e2e -- ohos`) then run in parallel on separate runners. They use the
build's `stage` artifact, which holds the release tarball, the test probes and
`manifest.tsv`. The tarballs keep file modes and symlinks. The `checks` job is
the only required gate. It fails unless every other job succeeded, so a failed
dependency can't turn into a skipped job that counts as green. Use
`gh run view <run-id>` to inspect a run.

The `Release` workflow publishes the Flatpak bundles
([packaging/flatpak](../../packaging/flatpak/README.md#release)).

The runners use Ubuntu 26.04, which provides the GTK baseline, Mutter and
Flatpak. `setup.sh` is the only script that uses sudo, and it refuses to run
outside GitHub Actions. Tests run the stage through the development Flatpak
driver on a `tests/nested.sh` desktop with headless Mutter and llvmpipe. As in
local runs, the sessions are named and memory capped. The workflow files explain
what each cache is keyed on and why. Android session data is never cached. No
Android image or third-party APK is uploaded as an artifact.
