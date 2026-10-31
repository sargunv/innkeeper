# innkeeper

The scriptable user surface: `bin/innkeeper`, built by `mise run build:host`,
run from a shell or from the `Exec` of the desktop entries it writes. Shared
operations are in [innkeeper-core](../core/README.md); app settings and file
grants go through the session's `Session1` D-Bus API.

`innkeeper --help` lists the commands; this covers what it doesn't say.

- Every command starts the session if needed; the first start downloads the
  profile's image, with progress on stderr.
- `profile create --system ohos NAME` makes an OpenHarmony profile
  ([ohos](../../ohos/README.md)). It refuses the app commands (`install`, `run`,
  `open`, `list`, `uninstall`, `app`) and the Android-only settings (adb, idle
  timeout, mirroring, desktop following).
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
  removals show at once. APKs and bundles are refused: `install` takes them. The
  desktop opens those with the manager; Innkeeper's desktop entry claims no
  document types, so it never becomes their default handler.
- `run` on a stopped session shows a splash window at once (`src/splash.rs`) and
  passes the desktop's `XDG_ACTIVATION_TOKEN` on to the app's window
  ([display](../../docs/display.md#launch-feedback)).
- `app set --background on` keeps the session up while that app is installed
  ([session](../../docs/session.md#lifecycle)).
- `uninstall` refuses system packages; `--keep-data` keeps `/data/data/<pkg>`.
- `--profile NAME` selects a profile; without it, commands work with `default`,
  created the first time ([session](../../docs/session.md#profiles)). App
  entries run `innkeeper --profile NAME run PKG`, so they keep their profile.
- The supported desktops have no standard hook for sharing text from the host,
  so the desktop entry accepts files only.

Tests: `cargo test -p innkeeper`, plus core's tests in `mise run test`.
