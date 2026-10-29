# tests

Cross-module tests; they run on the host and are never shipped. Module-local
tests live next to their code. Every tier needs `mise run build` (the unit tier
for the staged launcher), and the session tiers a prepared image. Their logs go
under `out/test/<suite>/`. Session suites use `tools/dev/session.sh`: real
Flatpak by default, private installation and HOME/XDG directories under `out/`,
and host-side 6/8 GiB scope limits. Set `LINUXAPK_MODE=host` to compare the same
stage through the host launcher. Set `LINUXAPK_INSTANCE` to a unique name;
suites stop and reset the data they create. Run windowed compat through
`tests/nested.sh mise run test:compat -- --window`; it cleans up its private
KWin, bus, runtime and XDG dirs. CLI tests automatically use that desktop. GTK
DynamicLauncher can be selected for tests with
`LINUXAPK_TEST_LAUNCHER_PORTAL=gtk`. Its dialogs are confirmed through AT-SPI
only on that private desktop.

| Tier       | Task                     | What                                                                                                                                                      |
| ---------- | ------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| unit       | `mise run test`          | Rust workspace, the broker fuzzer (bounded), the system app's `:protocol`, the HAL and bridge-client host tests, and the supervisor and image shell tests |
| bionic     | `mise run test:bionic`   | Android binaries in a fresh Flatpak session; host comparison uses the minimal bootless runner                                                             |
| binder     | `mise run test:binder`   | AOSP binder tests on the broker; kernel binder oracle is unavailable in Flatpak (exit 77)                                                                 |
| boot       | `mise run test:boot`     | a fresh headless boot plus `selftest`; host mode also checks both uid modes                                                                               |
| e2e        | `mise run test:e2e`      | audio, system-app and CLI suites on an isolated nested KWin (`e2e/run.sh`)                                                                                |
| a11y       | `mise run test:a11y`     | Android apps on AT-SPI in a nested, isolated KWin with its own AT-SPI bus, checked with pyatspi (`a11y/run.sh`)                                           |
| compat     | `mise run test:compat`   | the 31-app F-Droid corpus ([compat](compat/README.md))                                                                                                    |
| host state | `tests/hoststate/run.sh` | battery, power saver, metering, live locale and location against python-dbusmock and a mock Location portal on private buses (`--deny`: a portal refusal) |

- **`bionic/run.sh`** runs binaries in the actual Flatpak runtime. `-v` supplies
  an addon vendor layer, `-t` streams test files to `/data/nativetest64`, and
  `LINUXAPK_PROPS` entries become real properties. Frame dumps go to
  `out/test/bionic/<instance>/frames`. The host comparison retains the bootless
  image-rooted namespace and props shim.
- `tests/lifecycle.sh` uses the unshipped probes APK to check foreground audio
  with no activity, idle shutdown, and an alarm rescheduled on boot. It uses an
  isolated nested desktop and a private audio sink.
- The e2e suites install `out/test/probes/LinuxApkProbes.apk` from the system
  app's build; the audio suite routes to a private null sink.

`LINUXAPK_TEST_UPDATE=1 mise run test:e2e` also refreshes the private Flatpak
deployment and checks an existing menu entry after that update.
`tools/dev/benchmark.sh` compares fresh/cold boot, idle memory, Binder
throughput and gfxinfo for OpenCalc, Wikipedia and Tomato in isolated KWin.
