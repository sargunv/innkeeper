# Tests

What each tier tests is in [AGENTS.md](../AGENTS.md); move a variant down to a
cheaper tier before removing it from e2e, keeping every bug regression.

- `mise run test`: Rust, Kotlin protocol/input, HAL host tests, supervisor,
  image scripts and harness units. No Android session or APK corpus needed.
- `mise run test:e2e -- [group...]`: one fresh named session on an isolated
  desktop. No arguments runs five groups: `boot` (host safety, HOME and
  storage), `input`, `audio`, `desktop` (including sharing) and `lifecycle`.
  Build and prepare first:

  ```sh
  mise run build
  INNKEEPER_PROFILE=prepare-tests mise run session -- prepare
  INNKEEPER_PROFILE=e2e-local mise run test:e2e
  ```

- `mise run test:e2e -- ohos`: boots OpenHarmony ([ohos](../ohos/README.md)),
  fetching its image first, until SceneBoard is ready. It is its own session
  with no desktop window, so it runs alone.

The runner owns a private desktop
([tests/nested.sh](../docs/development.md#a-nested-isolated-desktop)) with fake
portals and notifications, and stops and resets its profile on exit, also on
failure.

Local e2e has a 55 s deadline plus at most 4 s cleanup, and groups wait for
observations, never fixed delays. Audio runs after the window checks, since
AudioRecord needs a visible activity. Setup waits for user 0's
`RUNNING_UNLOCKED` before installing probes: boot completion can precede
credential-encrypted app-data preparation. CI runs headless Mutter with llvmpipe
(`INNKEEPER_TEST_COMPOSITOR=mutter LIBGL_ALWAYS_SOFTWARE=1`) and a 180 s
deadline (`INNKEEPER_E2E_DEADLINE`); observation and group limits scale with it.
Logs, captures and phase timings land in `out/test/e2e/<profile>/` and desktop
logs in `out/test/nested/<profile>/`; CI uploads both as `checks-logs`. The
session runs without `INNKEEPER_TEST`, as a user's would: the nested desktop's
fake portals and notifications (`tests/e2e/desktop.py`) stand in for the real
desktop.

- `mise run test:bionic`: manual Android-side HAL diagnostics.
- `mise run test:compat -- --only <package>`: manual app troubleshooting; see
  [compat/README.md](compat/README.md). It refuses to run the whole corpus.
