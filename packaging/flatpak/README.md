# flatpak

A prototype Flatpak of Linux.apk (issue #25): `build.sh` takes the staged prefix
(`out/stage`, after `mise run build`) as `/app`, on the
`org.freedesktop.Platform//26.08` runtime (glibc 2.44; the host build links
against 2.43), and installs `dev.linuxapk.Manager` from a local repo into the
current user's installation. It builds nothing from source and ships no Android
image; the session fetches that at first use, as outside Flatpak.

```sh
mise run build
FLATPAK_USER_DIR=$PWD/out/flatpak/user packaging/flatpak/build.sh
flatpak run --command=/app/libexec/linuxapk/linuxapk-session \
  --env=LINUXAPK_INSTANCE=test dev.linuxapk.Manager start -f --headless
```

The session runs inside the sandbox
([Flatpak mode](../../docs/session.md#flatpak-mode)). Its permissions:
`--device=dri` (the render node and `/dev/udmabuf`; `--device=all` would bind
the host's `/dev` over the sandbox's writable one), `--share=network`,
`--socket=wayland`, `--socket=pulseaudio` and `--share=ipc`. Never
`flatpak-spawn --host` or `--talk-name=org.freedesktop.Flatpak`.

Flatpak moves `flatpak run` into its own systemd scope, out of any unit it was
started in; to cap a test session's memory, set `MemoryMax` on that
`app-flatpak-dev.linuxapk.Manager-*.scope`.
