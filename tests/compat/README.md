# compat

The app-compatibility gate: crate `linuxapk-compat`, run with
`mise run test:compat` (usage in the header of `src/main.rs`). It installs each
app of [corpus.toml](corpus.toml) (pinned F-Droid APKs, fetched to
`out/test/compat/corpus` and never committed) into a session, and records the
results in `results.json` and
[docs/compatibility.md](../../docs/compatibility.md).

Per app (`src/app.rs`): `pm install`, launch through the window manager, watch 8
s (15 s for games) for crashes, ANRs and death, check window focus, take a
screenshot that must not be blank (one colour over 99.5%), run the optional
script (syntax at the top of `corpus.toml`), then tap, scroll and Back, and scan
the log for known failure signatures (MediaCodec, WebView, GL/EGL/Vulkan errors,
Play services). Logs, screenshots and results go to
`out/test/compat/<instance>/<pkg>/` and
`out/test/compat/<instance>/results.json`. Separate named instances can run
concurrently without overwriting evidence.

- **Results:** works, partly works (the first frame was fine, a later check
  failed), or doesn't work. A session restart during an app is retried once and
  never counts against it.
- **Gate:** an app worse than in `results.json`, or a measurement over its
  budget by more than its margin ([budgets.toml](budgets.toml): cold session
  start, warm app start, idle session memory), fails the run and leaves the
  baseline alone. Only `--accept` updates the committed baseline and report.
  `-- report` rewrites `docs/compatibility.md` from `results.json`.
- **Session:** the named instance `LINUXAPK_INSTANCE` (default `compat`) if it
  is running, or else one it starts (headless unless `--window`) and stops at
  the end. Only a session it starts gets its start time and idle memory
  measured: it boots, restarts to time a cold start, and idles 90 s. A named
  instance is a test session, so links are only logged. Run it at nice 0:
  uiautomator dies when it can't reset its thread priority, so the harness
  re-runs itself at nice 0 when it finds itself niced.
- **Frameworks** in the corpus were identified by hand from each APK's libraries
  and assets (`libflutter.so`, `libhermes.so`, `assets/capacitor.config.json`,
  `libgodot_android.so`, Compose classes, `libgdx.so`, `libmaplibre.so`); there
  is no FOSS Unity or Xamarin app to test.
- **Triage:** a failing app gets a GitHub issue with the failure signature from
  its `out/test/compat/<instance>/<pkg>/` logs; `manual` notes in the corpus
  list checks the harness doesn't automate yet.

The default driver uses the current stage in a real Flatpak sandbox. For a
windowed run use
`LINUXAPK_INSTANCE=compat-window tests/nested.sh mise run test:compat -- --window`.
`LINUXAPK_MODE=host` selects the host comparison. The Flatpak has no TUN device,
so Rethink's VPN check currently reports partly working; its UI still renders.
The arm64-only Open Sudoku APK cannot install without binary translation.
Budgets remain unchanged for both modes.
