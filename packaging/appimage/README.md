# appimage

An AppImage of Linux.apk (issue #26): `build.sh` copies the staged prefix
(`out/stage`, after `mise run build`) to `AppDir/usr` and packs it with
appimagetool and the type2 runtime, both pinned in `mise.toml`. It builds
nothing from source and ships no Android image; the session fetches that at
first use, as from a checkout.

```sh
mise run build
packaging/appimage/build.sh            # out/appimage/Linux.apk-x86_64.AppImage
./Linux.apk-x86_64.AppImage            # the manager; open App.apk with it
./Linux.apk-x86_64.AppImage linuxapk install App.apk   # the CLI
```

An AppImage runs on the host, so its session is host mode's, as from a checkout
([docs/session.md](../../docs/session.md)):

- **Host libraries.** `AppRun` checks with `ldd` that this computer has the
  libraries the programs link (glibc, and GTK 4 and libadwaita for the manager)
  and names whatever is missing, in a notification too when there is no
  terminal. Nothing is bundled.
- **Host requirements.** `prepare` and `start` check what the session needs from
  the host (`host/launcher/src/requirements.rs`) and name all that is missing in
  one message, which the manager shows as a dialog. Each needs root to fix, so
  the message points to the Flatpak where that avoids it.
- **The session runs from a copy.** The AppImage's mount ends with the program
  that made it, and the session outlives that, so `start` copies the prefix to
  `$XDG_DATA_HOME/linuxapk/appimage/<build>/` once per build
  (`host/launcher/src/appimage.rs`).
- **Desktop entries** run apps through the AppImage
  (`Exec=<AppImage> linuxapk run <pkg>`): `AppRun` exports `$APPIMAGE` as
  `LINUXAPK_APPIMAGE`, which the session passes on to the bridge. When the
  AppImage moves or is replaced by a new file name (as Gear Lever does when it
  integrates one), the next manager start repairs the entries whose program is
  gone (`xdg::refresh_entries` in host/core).

Opening APKs from the file manager needs the manager's own desktop entry, which
an AppImage integrator such as Gear Lever installs from the one at the
AppImage's top.
