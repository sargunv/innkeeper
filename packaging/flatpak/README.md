# flatpak

The Linux.apk Flatpak, `dev.linuxapk.Manager`: the manager is its app (its
desktop entry opens APKs) and `linuxapk` its command, so
`flatpak run dev.linuxapk.Manager install App.apk` is the CLI. It ships no
Android image; the session fetches that at first use, as outside Flatpak.

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
