# linuxapk

The scriptable user surface: `bin/linuxapk`, built by `mise run build:host`, run
from a shell or from the `Exec` of the desktop entries it writes. All logic is
in [linuxapk-core](../core/README.md).

```sh
linuxapk install App.apk             # also .apks, .xapk, .apkm
linuxapk run org.wikipedia [URI]     # a window of its own, or focus the one it has
linuxapk uninstall org.wikipedia [--keep-data]
linuxapk list                        # user apps; repairs stale desktop entries first
```

- Every command starts the session if needed; the first start downloads the
  Android image (about 1.2 GB) with progress on stderr.
- `run` with a URI sends `VIEW` + `BROWSABLE` restricted to the package, so only
  activities that accept browser links get it; `file:`, `content:` and `intent:`
  URIs are refused.
- `uninstall` refuses system packages; `--keep-data` keeps `/data/data/<pkg>`.
- With `LINUXAPK_INSTANCE` set, entries bake it into their `Exec`, so a test's
  entries keep using its session.

Tests: `cargo test -p linuxapk` and the CLI suite of `mise run test:e2e`
(`tests/e2e/cli/run.sh`, which installs, launches through `gtk-launch` and
`gio open`, and uninstalls against a real session).
