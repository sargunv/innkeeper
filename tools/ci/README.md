# GitHub Actions

`CI` runs on pull requests and pushes to main: lint and one release build, then
unit tests and e2e in parallel on separate runners from that build's `stage`
artifact (release tarball, test probes and `manifest.tsv`; tarballs keep modes
and symlinks). The `checks` job is the single required gate: it fails unless
every job succeeded, so a failed dependency cannot turn into a green skip.
Inspect a run with `gh run view <run-id>`.

`Release` publishes the Flatpak bundles
([packaging/flatpak](../../packaging/flatpak/README.md#release)).

The runners are Ubuntu 26.04, which supplies the GTK baseline, Mutter and
Flatpak. `setup.sh` is the only script that uses sudo, and it refuses to run
outside GitHub Actions. Tests run the stage through the development Flatpak
driver on a `tests/nested.sh` desktop, headless Mutter with llvmpipe, so
sessions are named and memory capped as locally. The workflow files say what
each cache is keyed on and why. Android session data is never cached, and no
Android image or third-party APK is uploaded as an artifact.
