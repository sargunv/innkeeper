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
  connection timeout, CLI file VIEW and the Android Open With chooser),
  `sharing` (host SEND/SEND_MULTIPLE byte hashes, MIME matching and read-only
  grants), `a11y` (AT-SPI stays inactive), `production` (Android mailto VIEW and
  notifications reach logging desktop fakes, and PICK returns a readable file),
  and `lifecycle` (idle stop and alarm restart). The whole command, including
  setup and cleanup, has a 55 s deadline (plus at most 4 s for cleanup) on a
  warm image and prints its total wall time. Setup boots once and installs the
  probes; audio runs concurrently with the activity checks. Groups wait for
  observations, rather than fixed delays. Lifecycle uses named-session-only
  short timers.

Build and prepare the image first (`mise run build`,
`mise run session -- prepare`). Results and captures stay in `out/test/e2e/`.
The runner stops and resets its own instance on exit, including on failure. It
never uses the main session or the user's desktop configuration.

The `production` group runs after isolation checks and temporarily puts a
separately capped bridge on the named session's socket, leaving existing
display/audio connections on the test bridge. The sidecar uses normal production
policy on the nested desktop's private bus/runtime/XDG dirs, with no compositor
connection or host system bus. It restores the test socket and drops sidecar
connections on exit; the launcher still enforces test policy for every named
instance. The fake portals and notification server never launch desktop apps or
dialogs.

`mise run test:compat -- --only <package>` is a manual troubleshooting harness;
[compat/README.md](compat/README.md) describes its artifacts. The full corpus
runs only in nightly CI; it never gates an agent loop or a PR. Never trigger,
run, or wait for that job. Nightly regressions become issues.

`test:bionic` and `test:binder` are manual low-level HAL/broker diagnostics.
