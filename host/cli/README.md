# innkeeper

The scriptable user surface: `bin/innkeeper`, built by `mise run build:host`,
run from a shell or from the `Exec` of the desktop entries it writes. Shared
operations are in [innkeeper-core](../core/README.md); file grants use the
bridge's IPC client.

```sh
innkeeper install App.apk             # also .apks, .xapk, .apkm
innkeeper open [FILE...]              # choose an Android app; no files opens the manager
innkeeper run org.wikipedia [FILE...|URI]     # a window of its own, or focus the one it has
innkeeper uninstall org.wikipedia [--keep-data]
innkeeper list                        # user apps; repairs stale desktop entries first
innkeeper app org.wikipedia [ACTION]  # per-app info and settings, as JSON
innkeeper background [PKG on|off]     # may PKG run with no window open (lists them)
innkeeper profile list                # profiles and whether they run
innkeeper profile create|use|delete NAME  # delete removes its apps, data and settings
innkeeper profile rename OLD NEW
innkeeper profile start|stop [NAME]
innkeeper profile set [NAME] --adb on|off     # adb on a loopback port, live
innkeeper profile set [NAME] --mode phone --size medium-phone  # one device window, next start
innkeeper adb                         # have your adb list the profile (emulator-<port - 1>)
innkeeper --profile NAME install App.apk      # any command, on another profile
```

- Every command starts the session if needed; the first start downloads the
  Android image (about 1.2 GB), with progress on stderr.
- `run` with a URI sends `VIEW` + `BROWSABLE` restricted to the package, so only
  activities that accept browser links get it; `content:` and `intent:` URIs are
  refused. A path or `file:` URI is instead opened read-only and its fd handed
  to the session's documents service: Android gets an opaque `content://` URI,
  the MIME type and `FLAG_GRANT_READ_URI_PERMISSION`, for that package only,
  until the session stops. No host directory is exposed, and the file must
  already be accessible to the CLI's Flatpak sandbox.
- `open` grants files the same way, then shows **Open or share with Android
  app…**. **Open** lists VIEW handlers; **Share** lists SEND targets for one
  file and SEND_MULTIPLE targets for several, and the same activity must accept
  every file's MIME type. Targets are queried each time, so installs and
  removals show at once. APKs and bundles go to the installer instead.
  Innkeeper's desktop entry accepts common document, text and media types, and
  Flatpak forwards its `%F` through the document portal. No file-type defaults
  are changed.
- `run` on a stopped session shows a splash window at once (`src/splash.rs`) and
  passes the desktop's `XDG_ACTIVATION_TOKEN` on to the app's window
  ([display](../../docs/display.md#launch-feedback)).
- `background` keeps the session up while an allowed app is installed
  ([session](../../docs/session.md#lifecycle)).
- `uninstall` refuses system packages; `--keep-data` keeps `/data/data/<pkg>`.
- A profile is a separate Android with its own apps, data and settings
  ([session](../../docs/session.md#profiles)). `--profile NAME` or
  `INNKEEPER_PROFILE` selects one, else the one in use (`profile use`); the
  first command that needs one creates `default`. Desktop entries name their
  profile in `Exec`, so an app's entry keeps using it.
- The supported desktops have no standard hook for sharing text from the host,
  so the desktop entry accepts files only.

Tests: `cargo test -p innkeeper`, plus core's tests in `mise run test`.
