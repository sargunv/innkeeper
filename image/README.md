# image

The one Android image Innkeeper boots: an unmodified AOSP build from Android's
public CI ([pin.toml](pin.toml): `aosp_cf_x86_64_only_phone-userdebug`, Android
17, test-keys), downloaded from Google on the user's machine and never
redistributed or committed. `mise run build:data` stages the config to
`share/innkeeper/image/` and the scripts to `libexec/innkeeper/image/`;
`mise run build:image-tools` builds the static `simg2img`, `lpunpack`,
`fsck.erofs` and `debugfs` (`tools/build.sh`, in the build container), so setup
needs neither podman nor distro packages. `innkeeper-session prepare` (or the
first `start`) runs the pipeline.

```mermaid
flowchart LR
  fetch["fetch.sh<br/>download, size + sha256 check"] --> extract["extract.sh<br/>super.img → partitions"]
  extract --> flatten["flatten-apex.sh<br/>APEXes → dirs, per bootconfig"]
  flatten --> derive["derive.sh<br/>the derived layer"]
```

Cache, shared by every profile, in `$XDG_DATA_HOME/innkeeper/images/<name>/`:
`dl/` (resumable downloads; the zip goes once extracted), `img/` (the
partitions), `apex-<hash>/` (one flattened tree per vendor APEX selection, never
rewritten under a running session), `derived/` (the layer
[docs/session.md](../docs/session.md#overlay-stack) stacks above the image).

A successful `prepare` prunes stale builds (with their derived layer) and
unselected `apex-<hash>` trees, logging what it frees. The current pin and every
running session hold leases on their build and APEX tree, and a cache-wide lock
in `images/.locks/` serializes preparation and pruning across profiles. Pruning
skips symlinks, unknown directories and incomplete APEX extractions. A changed
derived layer can't be regenerated while another session uses that build; stop
it first.

- **apexd and flatten-apex.sh must agree.** [bootconfig](bootconfig) selects
  multi-install vendor APEXes the way cuttlefish's launcher does
  (`androidboot.vendor.apex.<name>=<file>|none`); the session passes the same
  values to init. A mismatch aborts apexd in `getActivePackages`.
- [vendor.prop](vendor.prop) overrides image properties; every line says why.

## Bumping the pin

1. Pick a build on ci.android.com (branch `aosp-android-latest-release`, the
   target above) and record the img zip's `url`, `size` and `sha256` in
   `pin.toml` with the new `name` and `fingerprint`.
2. `mise run session -- prepare`, then list `img/vendor/apex` and add a
   selection to `bootconfig` for every new multi-install or VM-only APEX.
3. `prepare` checks the upstream SHA-256 bases in
   [fork-bases.sha256](fork-bases.sha256) for the overlay's forks and overridden
   services, and fails with each changed path and an image-to-overlay diff or
   the file holding its override stanzas. Port upstream changes into the files
   listed in [android/overlay/README.md](../android/overlay/README.md), then
   update the hashes from the **image copies**, never the edited overlays.
   System paths resolve under `img/system/system/`; APEX paths under the
   selected `apex-<hash>/`. Stage the updated hashes with `mise run build`, then
   rerun `prepare`. Rename the blanked APEX APKs to the new build id.
4. Prove it: `mise run test:e2e` passes, and a few corpus apps still work
   (`mise run test:compat -- --only <package>,...`).

Tests: launcher unit tests cover pin selection and pruning.
`tests/fork-drift.sh` (in `mise run test`) checks the fork-base check against a
fixture; `prepare` checks the real bases against the pinned image.
