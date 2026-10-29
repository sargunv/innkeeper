# linuxapk

The scriptable user surface: `bin/linuxapk`, built by `mise run build:host`, run
from a shell or from the `Exec` of the desktop entries it writes. All logic is
in [linuxapk-core](../core/README.md).

```sh
linuxapk install App.apk             # also .apks, .xapk, .apkm
linuxapk run org.wikipedia [URI]     # a window of its own, or focus the one it has
linuxapk uninstall org.wikipedia [--keep-data]
linuxapk list                        # user apps; repairs stale desktop entries first
linuxapk background [PKG on|off]     # may PKG run with no window open (lists them)
```

- Every command starts the session if needed; the first start downloads the
  Android image (about 1.2 GB) with progress on stderr.
- `run` with a URI sends `VIEW` + `BROWSABLE` restricted to the package, so only
  activities that accept browser links get it; `file:`, `content:` and `intent:`
  URIs are refused.
- `run` on a stopped session shows a splash window at once (`src/splash.rs`) and
  passes the desktop's `XDG_ACTIVATION_TOKEN` on to the app's window
  ([display](../../docs/display.md#launch-feedback)).
- `background` keeps the session up while an allowed app is installed
  ([session](../../docs/session.md#lifecycle)).
- `uninstall` refuses system packages; `--keep-data` keeps `/data/data/<pkg>`.
- With `LINUXAPK_INSTANCE` set, entries bake it into their `Exec`, so a test's
  entries keep using its session.

Tests: `cargo test -p linuxapk` and the shared core tests in `mise run test`
cover command results, metadata, validation and desktop entries.
