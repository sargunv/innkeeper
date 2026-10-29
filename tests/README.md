# tests

Two gate tiers, both testing observable behaviour we own:

- `mise run test`: unit tests, without an Android session or compat APKs. Rust,
  Kotlin input/protocol, HAL host tests, supervisor and image scripts.
- `mise run test:e2e -- [group...]`: one runner on a disposable KWin, private
  D-Bus/runtime/XDG dirs, a fresh named Flatpak session, and our probes APK.
  With no arguments it runs all groups: `boot` (including host safety selftest),
  `storage` (mounted primary volume, external app directories, and a shared
  image scanned, queried and read through MediaStore before and after a remount
  that preserves MediaProvider), `input` (an app window and delivered input),
  `audio` (440 Hz at PipeWire), `desktop` (links and notifications logged,
  clipboard isolated both ways, cold-start file picker cancelled without a
  portal, and DocumentsUI after a connection timeout, CLI file VIEW and the
  Android Open With chooser), `sharing` (host SEND/SEND_MULTIPLE byte hashes,
  MIME matching and read-only grants), `a11y` (AT-SPI stays inactive),
  `production` (Android mailto VIEW and notifications reach logging desktop
  fakes, and PICK returns a readable file), and `lifecycle` (idle stop and alarm
  restart). Locally the whole command has a 55 s deadline (plus at most 4 s for
  cleanup) on a warm image. GitHub Actions sets `LINUXAPK_E2E_DEADLINE=180` for
  its four-vCPU software-rendered desktop; that override is ignored outside
  GitHub Actions. Setup observation waits scale from 20 s at the local 55 s
  deadline to 66 s at CI's 180 s deadline, bounded by the total deadline. Group
  observation waits remain 20 s. Setup holds the session's busy lock from the
  host during boot polling, then hands it over to the Android keeper once boot
  completes. The host lock closes before groups start, so lifecycle still tests
  idle shutdown when the keeper exits. The runner prints its total wall time and
  each setup phase and group's elapsed time/status, including interrupted
  phases, and saves those timings in `out/test/e2e/<instance>/timings.log`. CI
  uploads that directory in `checks-logs` even on failure. Setup boots once and
  installs the probes; audio runs concurrently with the activity checks. Groups
  wait for observations, rather than fixed delays. Lifecycle uses
  named-session-only short timers.

Build and prepare the image first (`mise run build`,
`mise run session -- prepare`). Results and captures stay in `out/test/e2e/`.
The runner stops and resets its own instance on exit, including on failure. It
never uses the main session or the user's desktop configuration.

The CI desktop uses KWin's OpenGL compositor with Mesa software rendering
(`KWIN_COMPOSE=O2 LIBGL_ALWAYS_SOFTWARE=1`). QPainter (`KWIN_COMPOSE=Q`) does
not advertise linux-dmabuf, so Android's windows cannot map there. KWin's
virtual OpenGL backend also needs a DRM device: CI provisions vgem, while local
development uses the host's existing device. The nested runner forwards renderer
overrides only to its private desktop and saves KWin's support information in
`kwin-support.txt`. CI fails before starting Android if KWin fell back from
OpenGL. To reproduce locally with a prepared image and CI stage:

```sh
GITHUB_ACTIONS=true LINUXAPK_E2E_DEADLINE=180 \
  KWIN_COMPOSE=O2 LIBGL_ALWAYS_SOFTWARE=1 LINUXAPK_INSTANCE=ci-repro \
  mise run test:e2e
```

Failed groups save bounded Android logs and session cgroup memory counters
before subsequent groups can clear logcat or restart the session. The nested
desktop saves its last 200 journal lines in `compositor.log` on exit.

Set `LINUXAPK_SHARE_SCREENSHOT=1` when capturing chooser proof; normal sharing
checks reuse the receiver window and exercise both chooser selections without
starting the screenshot tool.

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
run, or wait for that job. The
[compatibility wiki](https://github.com/sargunv/linux.apk/wiki/Compatibility)
publishes its results. The wiki renderer can be tested without a session or
APKs: `python3 -B -m unittest discover -s tools/ci/tests`.

Wiki publishing requires the repository Actions secret `LINUXAPK_WIKI_TOKEN`: a
classic personal access token from an account with write access to
`sargunv/linux.apk`, with the minimum `public_repo` scope for this public
repository. `GITHUB_TOKEN` cannot push to the separate wiki Git repository. If
the secret is absent, CI skips publishing with a notice; results remain in the
artifact. Wiki publishing errors never change the compatibility result.

`test:bionic` provides manual low-level HAL diagnostics.
