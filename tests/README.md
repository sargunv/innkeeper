# tests

Cross-module tests; they run on the host and are never shipped. Module-local
tests live next to their code. Every tier needs `mise run build` (the unit tier
for the staged launcher), and the session tiers a prepared image. Their logs go
under `out/test/<suite>/`. `boot` makes a fresh `/data` under `out/test/boot`;
`e2e` and `compat` run a named instance (`e2e`, or `LINUXAPK_INSTANCE`) on that
instance's usual `/data`, so set `LINUXAPK_DATA` for a fresh one.

| Tier   | Task                   | What                                                                                                                                                      |
| ------ | ---------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| unit   | `mise run test`        | Rust workspace, the broker fuzzer (bounded), the system app's `:protocol`, the HAL and bridge-client host tests, and the supervisor and image shell tests |
| bionic | `mise run test:bionic` | Android test binaries against our HALs and drivers without booting (`bionic/run.sh`)                                                                      |
| binder | `mise run test:binder` | AOSP's binder gtests on the broker, then on kernel binder (`supervisor/tests/binder`)                                                                     |
| boot   | `mise run test:boot`   | a fresh headless boot plus `selftest`, in subuid and single-uid mode (`boot.sh`)                                                                          |
| e2e    | `mise run test:e2e`    | audio, system-app and CLI suites against one headless instance (`e2e/run.sh`)                                                                             |
| a11y   | `mise run test:a11y`   | Android apps on AT-SPI in a nested, isolated KWin with its own AT-SPI bus, checked with pyatspi (`a11y/run.sh`)                                           |
| compat | `mise run test:compat` | the 31-app F-Droid corpus ([compat](compat/README.md))                                                                                                    |

- **`bionic/run.sh`** is the one runner for on-device binaries: it builds an
  image-rooted namespace (the image's `/system` and flattened `/apex`, the
  stage's `/vendor`, a generated `/linkerconfig`) and runs the program under
  pivot-exec and the broker, with servicemanager and our graphics HALs up.
  `props-shim.c` stands in for the property service and mirrors logs to stderr.
  The run is a capped systemd user unit.
- The e2e suites install `out/test/probes/LinuxApkProbes.apk` from the system
  app's build; the audio suite routes to a private null sink.
