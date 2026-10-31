# flatpak

The Linux.apk Flatpak, `dev.linuxapk.Manager`: the manager is its app (its
desktop entry opens APKs in the installer and documents in an Android app
chooser) and `linuxapk` its command, so
`flatpak run dev.linuxapk.Manager install App.apk` is the CLI. It ships no
Android image; the session fetches that at first use, on first use.

```sh
FLATPAK_USER_DIR=$PWD/out/flatpak/user packaging/flatpak/build.sh
FLATPAK_USER_DIR=$PWD/out/flatpak/user flatpak run dev.linuxapk.Manager list
```

`build.sh` builds from the committed tree (it refuses uncommitted changes):

- **The desktop side** (CLI, manager, bridge, launcher) is built by
  flatpak-builder on the `org.gnome.Platform` runtime from `git archive HEAD`
  and the vendored crates, offline.
- **The Android side** (bionic and static binaries, the overlay, the system app,
  the image tools) comes from `mise run build`, which needs the NDK and the
  build container; none of it links the runtime.
- flatpak-builder runs as `org.flatpak.Builder` from a second private
  installation (`out/flatpak/tools`). Both installations get Flathub as a
  remote, and the result is installed or updated from a local repo
  (`out/flatpak/repo`).

## Release

[`.github/workflows/release.yml`](../../.github/workflows/release.yml) publishes
two channels into one OSTree repo. A push to `main` exports the Flatpak branch
`snapshot` (the channel to install; a newer push cancels the one in progress). A
pushed `v*` tag exports the branch `stable`. It reuses the CI stage cache
(`mise run build` only on a miss), then `packaging/flatpak/release.sh` exports.
The app id comes from the manifest, the remote name from the id's last
component, and the Pages path from the repository name (`origin`'s last path
segment, `github.event.repository.name` in CI), so a rename moves all three.

The workflow publishes two ways:

- **A bundle** (`<app>-snapshot.flatpak` or `<app>-stable.flatpak`), built with
  `flatpak build-bundle --repo-url=https://code.sargunv.dev/<repository>/repo`
  and `--runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo`. A file
  install records the Pages repo, so `flatpak update` follows the channel. The
  GNOME runtime comes from Flathub. A snapshot replaces the asset on one
  floating prerelease named `snapshot`, and the workflow force-moves the
  `snapshot` tag to that commit; the release notes say which commit it is and
  when it was built. A `v*` tag gets its own release, created with generated
  notes when it does not exist.
- **A static OSTree repo on GitHub Pages**, at
  `https://code.sargunv.dev/<repository>/`. Each publish commits into the
  restored repo (`flatpak build-export` on `snapshot` or `stable`, then
  `flatpak build-update-repo --generate-static-deltas --prune`) and pushes the
  `gh-pages` branch. Pages deploys from that branch, so both channels' history
  persists; `actions/deploy-pages` would replace the site and drop it. The
  branch also carries `<remote>.flatpakrepo`, one `.flatpakref` per branch
  (`<app>-snapshot.flatpakref`, `<app>.flatpakref`) and `index.html`.

A snapshot's version is `0.0.<git rev-list --count HEAD>`, with the short SHA in
the commit subject. The package job checks out the full history so that count is
the commit count. A tag's version is the tag, without the leading `v`.

Signing uses `FLATPAK_GPG_PRIVATE_KEY` (ASCII-armored, no passphrase) and
`FLATPAK_GPG_KEY_ID` from the **`release` environment**, not repository secrets.
The environment allows deployments from `main` and `v*` tags, and both publish
jobs declare it. The workflow exports the public key from the private key and
embeds it (base64) in the `.flatpakrepo` and `.flatpakref`. A publish with
either secret missing fails before export. `workflow_dispatch` defaults to a dry
run: it builds the snapshot channel and uploads nothing, skips the environment,
and does not sign.

Once, in the repository settings, set Pages to deploy from the `gh-pages` branch
(root) and the custom domain `code.sargunv.dev`. The first publish creates the
branch.

Users add the remote once and take later builds with `flatpak update`:

```sh
flatpak remote-add --if-not-exists <remote> https://code.sargunv.dev/<repository>/<remote>.flatpakrepo
flatpak install <remote> <app>//snapshot
flatpak install <remote> <app>//stable
```

Or install a bundle. It needs the Flathub remote for the runtime, and updates
from the Pages repo:

```sh
flatpak install ./<app>-snapshot.flatpak
```

`<app>` is the manifest `id`, `<remote>` its last component in lower case, and
`<repository>` the GitHub repository name.

The same export, unsigned and unpublished, for a local check. It defaults to the
snapshot channel; pass `stable` and `LINUXAPK_RELEASE_VERSION` for the other:

```sh
mise run package:release
LINUXAPK_RELEASE_VERSION=0.1.0 packaging/flatpak/release.sh stable
```

That writes `out/release/repo`, `out/release/<app>-<channel>.flatpak` and
`out/release/publish/`. A second channel's run keeps the first branch in the
repo. Set both signing variables to sign a local export; the key is never
written to the repository.

The session runs inside the sandbox
([Flatpak mode](../../docs/session.md#flatpak-mode)): `start` in a sandbox of
its own through the Flatpak portal's Spawn, `start -f` in the caller's, and the
other commands from any instance of the app
(`--command=/app/libexec/linuxapk/linuxapk-session`). Permissions and what each
desktop service goes through:
[security.md](../../docs/security.md#flatpak-mode). `--device=dri` gives the
render node and `/dev/udmabuf`; `--device=all` would bind the host's `/dev` over
the sandbox's writable one. Never `flatpak-spawn --host` or
`--talk-name=org.freedesktop.Flatpak`.

Testing it on a nested desktop
([development.md](../../docs/development.md#a-nested-isolated-desktop)): set
`FLATPAK_USER_DIR` and add `$FLATPAK_USER_DIR/exports/share` to `XDG_DATA_DIRS`
in its environment, and pass `PULSE_SERVER` (the private runtime dir has no
Pulse socket). Flatpak forwards files as paths under the host's
`$XDG_RUNTIME_DIR/doc`, which the sandbox only has at `/run/user/$UID/doc`, so
with a private runtime dir an opened APK is missing unless that `doc` dir is
also granted (`flatpak run --filesystem=<runtime dir>/doc`). Flatpak moves
`flatpak run` into its own systemd scope; to cap a test session's memory, set
`MemoryMax` on that `app-flatpak-dev.linuxapk.Manager-*.scope`.

## Development

`mise run session -- ...` and the session test suites run the stage in this
sandbox through `tools/dev/session.sh` (`tools/dev/linuxapk.sh` for the CLI),
without building the Flatpak:

- The first use runs `dev-init.sh`, which installs a snapshot of `out/stage`
  into the private `out/flatpak/user`. Each run then mounts the current stage as
  `/app` (`flatpak run --app-path`) and grants it read-only, so sessions the
  portal spawns, alarm wakeups included, start the stage's files too, and the
  snapshot's `linuxapk` and `linuxapk-manager` forward to the stage for menu
  entries. Rerun `dev-init.sh` to test a Flatpak update.
- HOME and the XDG dirs are scratch dirs under `out/dev/flatpak/<profile>`; the
  image cache is the user's.
- The driver holds the sandbox at a gate until it has set `MemoryHigh=6G` and
  `MemoryMax=8G` on its scope from the host, and a host-side watcher caps the
  sandboxes alarm wakeups start. The app gets no systemd access for it.
- `status`, `stop` and `exec` talk to the session's control socket from the
  host, with no Flatpak per command.
- Stop a session before rebuilding the stage it runs.
