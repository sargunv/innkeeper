# innkeeper

`bin/innkeeper` is the command-line interface for users and scripts. It is built
by `mise run build:host`. It runs from a shell or from the `Exec` line of the
desktop entries it writes. Shared operations are in
[innkeeper-core](../core/README.md). App settings and file grants go through the
session's `Session1` D-Bus API.

`innkeeper --help` lists the commands. This file covers what the help text
doesn't say.

- Every command starts the session if it isn't running. The first start
  downloads the profile's image and shows progress on stderr.
- `profile create --system ohos NAME` makes an OpenHarmony profile
  ([ohos](../../ohos/README.md)). `install` takes `.hap` files there. `run`
  starts the bundle's launcher ability, `list` shows the bundles that the user
  installed, and `uninstall` refuses preinstalled bundles. `app stop`,
  `app clear-cache` and `app clear-data` use `aa force-stop` and `bm clean`.
  Such a profile refuses `open`, files and URIs in `run`, `app info`, `app set`,
  and the settings that only Android has (adb, idle timeout, desktop following).
  OpenHarmony apps get no desktop entries.
- `run` with a URI sends a `VIEW` intent with the `BROWSABLE` category to the
  package. Only activities that accept browser links receive it. `content:` and
  `intent:` URIs are refused.
- `run` with a path or `file:` URI opens the file read-only and hands its fd to
  the session's documents service. Android gets an opaque `content://` URI, the
  MIME type and `FLAG_GRANT_READ_URI_PERMISSION`. The grant is for that package
  and lasts until the session stops. No host directory is exposed. The file must
  already be accessible inside the CLI's Flatpak sandbox.
- `open` grants files the same way and then shows **Open or share**. **Open**
  lists `VIEW` handlers. **Share** lists `SEND` targets for one file and
  `SEND_MULTIPLE` targets for several. A target must accept the MIME type of
  every file. Targets are queried each time, so newly installed and removed apps
  show up immediately.
- `open` refuses APKs and bundles, which `install` takes. The desktop opens
  those files with the manager. Innkeeper's desktop entry claims no document
  types, so it doesn't become their default handler.
- `run` on a stopped session shows a splash window immediately
  (`src/splash.rs`). It passes the desktop's `XDG_ACTIVATION_TOKEN` on to the
  app's window ([display](../../docs/display.md#launch-feedback)).
- `app set --background on` keeps the session running while that app is
  installed ([session](../../docs/session.md#lifecycle)).
- `uninstall` refuses system packages. `--keep-data` keeps `/data/data/<pkg>`.
- `--profile NAME` selects a profile. Without it, commands use the `default`
  profile, which is created on first use
  ([session](../../docs/session.md#profiles)). App entries run
  `innkeeper --profile NAME run PKG`, so they keep their profile.
- The supported desktops have no standard way to share text with an app, so the
  desktop entry accepts only files.

`cargo test -p innkeeper` runs the tests. `mise run test` runs them along with
core's tests.
