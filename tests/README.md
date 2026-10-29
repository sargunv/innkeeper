# tests

Two gate tiers, both testing observable behaviour we own:

- `mise run test`: unit tests, without an Android session or compat APKs. Rust,
  Kotlin input/protocol, HAL host tests, supervisor and image scripts.
- `mise run test:e2e -- [group...]`: one runner on a disposable KWin, private
  D-Bus/runtime/XDG dirs, a fresh named Flatpak session, and our probes APK.
  With no arguments it runs all groups: `boot` (including host safety selftest),
  `input` (an app window and delivered input), `audio` (440 Hz at PipeWire),
  `desktop` (links and notifications logged, clipboard isolated both ways,
  cold-start file picker cancelled without a portal, and DocumentsUI after a
  connection timeout, and CLI file VIEW), `a11y` (AT-SPI stays inactive), and
  `lifecycle` (idle stop and alarm restart). The whole command, including setup
  and cleanup, has a 52 s deadline (plus at most 5 s for cleanup) on a warm
  image and prints its total wall time. Setup boots once and installs the
  probes; audio runs concurrently with the activity checks. Groups wait for
  observations, rather than fixed delays. Lifecycle uses named-session-only
  short timers.

Build and prepare the image first (`mise run build`,
`mise run session -- prepare`). Results and captures stay in `out/test/e2e/`.
The runner stops and resets its own instance on exit, including on failure. It
never uses the main session or the user's desktop configuration.

`mise run test:compat -- --only <package>` is a manual troubleshooting harness;
[compat/README.md](compat/README.md) describes its artifacts. The full corpus
runs only in nightly CI; it never gates an agent loop or a PR. Never trigger,
run, or wait for that job. Nightly regressions become issues.

`test:bionic` and `test:binder` are manual low-level HAL/broker diagnostics.
