# flatpak

This directory builds the Innkeeper Flatpak, `dev.sargunv.Innkeeper`. The
manager is the Flatpak's app, and its desktop entry opens APKs and bundles in
the installer. `innkeeper` is the Flatpak's command, so
`flatpak run dev.sargunv.Innkeeper install App.apk` runs the CLI. The Flatpak
ships no Android image. The session downloads one on first use.

```sh
FLATPAK_USER_DIR=$PWD/out/flatpak/user mise run package -- --install
FLATPAK_USER_DIR=$PWD/out/flatpak/user flatpak run dev.sargunv.Innkeeper list
```

`mise run package` runs `mise run build`. Then `build.sh` packs
`out/release/<app>-<channel>.flatpak` from the committed tree, and it refuses to
run with uncommitted changes. `--install` reinstalls that bundle into
`FLATPAK_USER_DIR`, which defaults to the user installation. The bundle has
these parts:

- **The desktop side** (CLI, manager, bridge, launcher) is built offline by
  flatpak-builder on the `org.gnome.Platform` runtime, from `git archive HEAD`
  and the vendored crates.
- **The Android side** (bionic and static binaries, the overlay, the system app,
  the image tools) comes from `mise run build`, which needs the NDK and the
  build container. None of it links against the runtime.
- flatpak-builder runs as `org.flatpak.Builder` from a private installation
  (`out/flatpak/tools`) and builds against an SDK in a second private
  installation (`out/flatpak/sdk`). Two installations are needed because Flatpak
  hides the installation an app runs from inside the app's sandbox.

## Release

[`.github/workflows/release.yml`](../../.github/workflows/release.yml) attaches
a single-file bundle to a GitHub Release. There is no Flatpak repository, so
users install each newer bundle by hand.

A successful CI run on a push to `main` builds `<app>-snapshot.flatpak` from the
stage that CI saved for that commit. A newer push cancels a snapshot build in
progress. The workflow uploads the bundle to a floating prerelease named
`snapshot`, deletes any other asset there, and force-moves the `snapshot` tag to
the commit. The release notes give the commit, the build time and the install
command.

A pushed `v*` tag builds `<app>-stable.flatpak` and attaches it to that tag's
release. The workflow creates the release with generated notes if it doesn't
exist. A `workflow_dispatch` run defaults to a dry run and uploads nothing.

The bundle is made with
`flatpak build-bundle --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo`,
so the GNOME runtime installs from Flathub. The bundle has no update repo. A
snapshot's version is `0.0.<git rev-list --count HEAD>`, and the commit subject
holds the short SHA. The package job checks out the full history so that the
count is correct. A tag's version is the tag without the leading `v`. The app id
comes from the manifest. A snapshot build restores the stage that CI saved for
the commit and fails if it is missing. A tag or a manual run builds the stage
when the cache has none.

Users install a bundle as described in the [README](../../README.md#install).

`mise run package` builds the same bundle locally on the snapshot channel and
doesn't publish it. `mise run package -- stable` needs a `v*` tag on HEAD.

The session runs inside the sandbox ([session.md](../../docs/session.md)).
`start` runs it in a separate sandbox through the Flatpak portal's Spawn method.
`start -f` runs it in the caller's sandbox. The other commands run from any
instance of the app (`--command=/app/libexec/innkeeper/innkeeper-session`).
[security.md](../../docs/security.md#the-sandbox) lists the permissions and the
path each desktop service takes. `--device=dri` provides the render node and
`/dev/udmabuf`. `--device=all` is not used because it would bind the host's
`/dev` over the sandbox's writable one. The Flatpak doesn't use
`flatpak-spawn --host` or `--talk-name=org.freedesktop.Flatpak`.

To test the Flatpak on a nested desktop
([development.md](../../docs/development.md#a-nested-isolated-desktop)), set
`FLATPAK_USER_DIR` in the nested desktop's environment and add
`$FLATPAK_USER_DIR/exports/share` to `XDG_DATA_DIRS`. Pass `PULSE_SERVER` too,
because the private runtime dir has no Pulse socket. Flatpak forwards opened
files as paths under the host's `$XDG_RUNTIME_DIR/doc`. The sandbox has that
directory only at `/run/user/$UID/doc`. With a private runtime dir, an opened
APK is therefore missing unless that `doc` dir is also granted
(`flatpak run --filesystem=<runtime dir>/doc`). Flatpak moves `flatpak run` into
its own systemd scope. To cap a test session's memory, set `MemoryMax` on that
`app-flatpak-dev.sargunv.Innkeeper-*.scope`.

## Development

`mise run session -- ...` and the session test suites run the stage in this
sandbox without building the Flatpak. They use `tools/dev/session.sh`, and
`tools/dev/innkeeper.sh` for the CLI.

- The first use runs `dev-init.sh`, which installs a snapshot of `out/stage`
  into the private `out/flatpak/user`. `tools/dev/session.sh bootstrap` runs
  `dev-init.sh` if the app isn't installed and then exits. Each later run mounts
  the current stage as `/app` (`flatpak run --app-path`) and grants it
  read-only. Sessions that the portal spawns, including alarm wakeups, therefore
  also start the stage's files. The snapshot's `innkeeper` and
  `innkeeper-manager` forward to the stage for menu entries. Rerun `dev-init.sh`
  to test a Flatpak update.
- HOME and the XDG dirs are scratch dirs under `out/dev/flatpak/<profile>`. The
  image cache stays in the user's home directory.
- A runtime systemd drop-in in
  `$XDG_RUNTIME_DIR/systemd/user/app-flatpak-dev.sargunv.Innkeeper-.scope.d/`
  sets `MemoryHigh=6G` and `MemoryMax=8G` on every scope that Flatpak creates
  for the app, including the portal's alarm wakeups. The dev runner checks that
  the session's scope got these limits. The drop-in also applies to the
  installed app until the next reboot. The app itself gets no systemd access for
  the limits. A start fails immediately when less than 10 GiB of memory is
  available.
- `status`, `stop` and `exec` talk to the session's control socket from the
  host, without starting a Flatpak instance per command.
- Stop a session before rebuilding the stage it runs.
