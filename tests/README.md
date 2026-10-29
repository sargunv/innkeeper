# Tests

Unit and low-level tests carry branches, edge cases and error handling; e2e
keeps one common-path scenario per user-visible feature. Move variants down
before removing them from e2e, preserving every bug regression at the cheapest
level.

- `mise run test`: Rust, Kotlin protocol/input, HAL host tests, supervisor,
  image scripts and harness units. No Android session or APK corpus needed.
- `mise run test:e2e -- [group...]`: one fresh named session on an isolated
  desktop. No arguments runs five groups: `boot` (host safety, HOME and
  storage), `input`, `audio`, `desktop` (including sharing) and `lifecycle`.
  Build and prepare first:

  ```sh
  mise run build
  LINUXAPK_INSTANCE=prepare-tests mise run session -- prepare
  LINUXAPK_INSTANCE=e2e-local mise run test:e2e
  ```

The runner owns private D-Bus/runtime/HOME/XDG dirs, a null audio sink and fake
portals/notifications. It stops and resets its instance on exit, including on
failure. Never use the main session or the user's desktop for tests. Check
`free -g` first; sessions are memory capped. See the
[nested desktop rules](../docs/development.md#a-nested-isolated-desktop).

Local e2e has a 55 s deadline plus at most 4 s cleanup. Groups wait for
observations; audio runs concurrently. CI uses headless Mutter/llvmpipe with
`LINUXAPK_TEST_COMPOSITOR=mutter LIBGL_ALWAYS_SOFTWARE=1` and a 180 s deadline
(`LINUXAPK_E2E_DEADLINE`, honored only with `GITHUB_ACTIONS=true`). Observation
and group limits scale with that deadline. Logs, captures and phase timings land
in `out/test/e2e/<instance>/`; desktop logs are in
`out/test/nested/<instance>/`. CI uploads these in `checks-logs`. Android log
captures are bounded. `desktop` uses a capped bridge sidecar on the same private
desktop to check real policy without opening host apps or dialogs.

- `mise run test:bionic`: manual Android-side HAL diagnostics.
- `mise run test:compat -- --only <package>`: manual app troubleshooting; see
  [compat/README.md](compat/README.md). The full corpus runs only in nightly CI.
  Never trigger, run or wait for it during development or PR work.
