# linuxapk

The scriptable user surface: `bin/linuxapk`, built by `mise run build:host`, run
from a shell or from the `Exec` of the desktop entries it writes. Shared
operations are in [linuxapk-core](../core/README.md); file grants use the bridge
IPC client.

```sh
linuxapk install App.apk             # also .apks, .xapk, .apkm
linuxapk open [FILE...]              # choose an Android app; no files opens the manager
linuxapk run org.wikipedia [FILE...|URI]     # a window of its own, or focus the one it has
linuxapk uninstall org.wikipedia [--keep-data]
linuxapk list                        # user apps; repairs stale desktop entries first
linuxapk background [PKG on|off]     # may PKG run with no window open (lists them)
```

- Every command starts the session if needed; the first start downloads the
  Android image (about 1.2 GB) with progress on stderr.
- `run` with a URI sends `VIEW` + `BROWSABLE` restricted to the package, so only
  activities that accept browser links get it; `content:` and `intent:` URIs are
  refused. Paths and `file:` URIs instead import a read-only file fd into the
  session's documents service. Android gets an opaque `content://` URI, MIME
  type and `FLAG_GRANT_READ_URI_PERMISSION`, restricted to that package. These
  grants last until the session stops; they expose no host directories. Files
  must already be accessible to the CLI's Flatpak sandbox.
- The static Linux.apk desktop entry accepts common document, text and media
  types. Flatpak exports its `%F` as `--file-forwarding … @@ %F @@`, granting
  those files through the document portal. `open` uses the same read-only grants
  as `run`, then shows **Open with Android app…** with installed apps whose
  exported VIEW filters match the file's MIME type. If none match, the chooser
  says so. APKs and bundles open in the installer instead. No file-type defaults
  are changed; per-app host Open With entries remain deferred.
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
