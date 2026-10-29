# tests

Cross-module tests; they run on the host and are never shipped. Module-local
tests live next to their code. Every tier needs `mise run build` (the unit tier
for the staged launcher), and the session tiers a prepared image. Their logs go
under `out/test/<suite>/`. The session suites run the stage in a private Flatpak
through `tools/dev/session.sh`
([packaging/flatpak](../packaging/flatpak/README.md#development)); each runs a
named instance (`LINUXAPK_INSTANCE`) on a fresh `/data` and resets it at the
end. The windowed ones (e2e, `--window` compat, `lifecycle.sh`) run themselves
on `nested.sh`'s disposable KWin, with a private bus, runtime dir and XDG dirs.
There e2e uses the GTK DynamicLauncher backend (`LINUXAPK_TEST_LAUNCHER_PORTAL`;
KDE's dialog fails to load on a nested KWin), whose dialogs `e2e/cli/confirm.py`
accepts over AT-SPI. In Flatpak mode the CLI suite opens a custom scheme through
`linuxapk run` rather than `gio open`: DynamicLauncher doesn't update the MIME
cache.

| Tier       | Task                      | What                                                                                                                                                                |
| ---------- | ------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| unit       | `mise run test`           | Rust workspace, the broker fuzzer (bounded), the system app's `:protocol`, the HAL and bridge-client host tests, and the supervisor and image shell tests           |
| bionic     | `mise run test:bionic`    | Android test binaries against our HALs and drivers, in a booted session with its framework stopped (`bionic/run.sh`)                                                |
| binder     | `mise run test:binder`    | AOSP's binder gtests on the broker, then on kernel binder where binderfs exists, so not in Flatpak (77) (`supervisor/tests/binder`)                                 |
| boot       | `mise run test:boot`      | a fresh headless boot plus `selftest`, in the Flatpak (`boot.sh`)                                                                                                   |
| e2e        | `mise run test:e2e`       | audio, system-app and CLI suites on an isolated nested KWin (`e2e/run.sh`)                                                                                          |
| a11y       | `mise run test:a11y`      | Android apps on AT-SPI in a nested, isolated KWin with its own AT-SPI bus, checked with pyatspi (`a11y/run.sh`)                                                     |
| compat     | `mise run test:compat`    | the 31-app F-Droid corpus ([compat](compat/README.md))                                                                                                              |
| lifecycle  | `mise run test:lifecycle` | foreground audio, idle shutdown and alarm wakeup on a private nested desktop (`lifecycle.sh`)                                                                       |
| host state | `mise run test:hoststate` | battery, power saver, metering, live locale and location against python-dbusmock (UPower) and mock host-state portals on private buses (`--deny`: a portal refusal) |

- **`bionic/run.sh`** is the one runner for on-device binaries. In Flatpak mode
  (`bionic/flatpak.sh`) it boots a session with `-v` dirs as an addon's vendor
  layer, stops Android's framework so the program has the composer to itself,
  copies `-t`'s dir to `/data/nativetest64` and runs the program with
  `session exec`; `LINUXAPK_PROPS` become real properties, except `ro.*` ones,
  which `props-shim.c` serves. With `LINUXAPK_BIONIC_HARNESS=1` it boots
  nothing: it builds an image-rooted namespace (the image's `/system` and
  flattened `/apex`, the stage's `/vendor`, a generated `/linkerconfig`) and
  runs the program under pivot-exec and the broker, with servicemanager and our
  graphics HALs up, and `props-shim.c` standing in for the property service.
  Either way it is capped.
- The e2e suites install `out/test/probes/LinuxApkProbes.apk` from the system
  app's build; the audio suite routes to a private null sink.
  `LINUXAPK_TEST_UPDATE=1` adds a Flatpak update under an existing menu entry
  and a cold launch's splash time (200 ms at most) to the CLI suite.
- `lifecycle.sh` checks with the probes that foreground audio outlives its
  activity and the idle grace, then that the session stops idle and an alarm set
  before it wakes the session and fires.
