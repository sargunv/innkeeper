# Tests

[AGENTS.md](../AGENTS.md) says what each tier tests. Before removing a variant
from e2e, move it to a cheaper tier. Keep every bug regression test.

- `mise run test` runs the Rust tests, the Kotlin protocol and input tests, the
  HAL host tests, the supervisor tests, the image script tests and the harness
  unit tests. It needs no Android session or APK corpus.
- `mise run test:e2e -- [group...]` runs one fresh named session on an isolated
  desktop. Without arguments it runs five groups: `boot` (host safety, HOME and
  storage), `input`, `audio`, `desktop` (including sharing) and `lifecycle`.
  Build first:

  ```sh
  mise run build
  INNKEEPER_PROFILE=e2e-local mise run test:e2e
  ```

- `mise run test:e2e -- ohos` fetches the OpenHarmony image and creates a fresh
  OpenHarmony profile ([ohos](../ohos/README.md)). It starts the profile on the
  private desktop and waits until SceneBoard is ready and its window shows. It
  also runs selftest. Boot ends on the home screen, and a click on the desktop's
  seat opens Settings with OpenHarmony's cursor under the pointer. A wheel
  detent scrolls Settings' list. OpenHarmony's input injector scrolls Settings
  and goes home, and render_service must not have restarted since boot. A key
  press reaches OpenHarmony's input service. A file written to the user's
  Download lands in the profile's data directory. The CLI installs the probe HAP
  (ohos/probe), signed for the profile's UDID, lists it and runs it, and its
  window has focus on screen. `app info` shows the probe's version and the grant
  state of its two permissions, and `run` with a link delivers its URI to the
  probe. OpenHarmony's music volume must be at its maximum, and a tone it plays
  must reach a private null sink. The probe, granted the microphone, must record
  a tone that the host plays into that sink. This group uses its own profile,
  `<profile>-ohos`, so it runs alone.

The runner starts a private desktop
([tests/nested.sh](../docs/development.md#a-nested-isolated-desktop)) with fake
portals and notifications. On exit, including on failure, it stops and resets
its profile.

Local e2e has a 55 s deadline plus at most 4 s for cleanup. Before the deadline
starts, the runner installs the dev Flatpak on the first run in a checkout and
prepares the image. Groups wait for observations instead of fixed delays. Audio
runs after the window checks because AudioRecord needs a visible activity. Boot
completion can come before Android prepares credential-encrypted app data, so
setup waits for user 0 to reach `RUNNING_UNLOCKED` before it installs the
probes.

CI runs headless Mutter with llvmpipe
(`INNKEEPER_TEST_COMPOSITOR=mutter LIBGL_ALWAYS_SOFTWARE=1`) and a 180 s
deadline (`INNKEEPER_E2E_DEADLINE`). Observation and group time limits scale
with the deadline.

Logs, captures and phase timings go to `out/test/e2e/<profile>/`. Desktop logs
go to `out/test/nested/<profile>/`. CI uploads both as `checks-logs`. The
session runs without `INNKEEPER_TEST`, like a user's session. The nested
desktop's fake portals and notifications (`tests/e2e/desktop.py`) stand in for
the real desktop.

- `mise run test:bionic` runs manual Android-side HAL diagnostics.
- `mise run test:compat -- --only <package>` is for manual app troubleshooting.
  See [compat/README.md](compat/README.md). It refuses to run the whole corpus.
