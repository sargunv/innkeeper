# flatpak

The Linux.apk Flatpak, `dev.linuxapk.Manager`: the manager is its app (its
desktop entry opens APKs) and `linuxapk` its command, so
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
- HOME and the XDG dirs are scratch dirs under `out/dev/flatpak/<instance>`; the
  image cache is the user's.
- The driver holds the sandbox at a gate until it has set `MemoryHigh=6G` and
  `MemoryMax=8G` on its scope from the host, and a host-side watcher caps the
  sandboxes alarm wakeups start. The app gets no systemd access for it.
- `status`, `stop` and `exec` talk to the session's control socket from the
  host, with no Flatpak per command.
- Stop a session before rebuilding the stage it runs.
