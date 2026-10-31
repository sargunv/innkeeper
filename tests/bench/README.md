# Manual measurement harness

`mise run bench -- input|frame [--noisy] [--seconds N] [--max-load N]` measures
input and frame latency, separate from `mise run test` and `test:e2e`. Build and
stage first (including `build:system-app`) and prepare the image. Each run makes
a fresh `bench-<mode>-<time>` profile on its own nested desktop, applies the
development memory caps and the safety selftest, and removes both after. It
refuses when one-minute load exceeds `--max-load` (default half the logical
CPUs) or another session is present; `--noisy` runs anyway and marks the result
noisy. For a session's memory, use `tools/dev/footprint.sh`.

| Mode    | Measures                                                                                                                    |
| ------- | --------------------------------------------------------------------------------------------------------------------------- |
| `input` | key press to commit and to `wp_presentation` of a probe's colour change, from the bridge trace (`INNKEEPER_BRIDGE_DEBUG=1`) |
| `frame` | commit and presentation intervals of a scrolling list and 200 moving sprites, `--seconds` each (default 5)                  |

Results land in `out/bench/<profile>/`: `result.json`, `summary.txt` and bounded
raw traces. A null statistic means no observation, never zero. Nothing is
committed or published, and measurements don't go in docs. Compare only runs
with the same release build, hardware, compositor and power settings; nested and
software compositors prove the harness works, not numbers. A virtual desktop
measures scanout estimates, not photons.

For another runtime the user already has (the Android Emulator, Waydroid),
install the probes APK with `adb install -r -g`, launch
`dev.sargunv.innkeeper.probes/.BenchProbe` with `--es mode input|scroll|game`,
and read `adb logcat -d -t 2000 -s InnkeeperBench:I`. Never install root
services, binderfs or kernel modules for a baseline.

`cargo test -p innkeeper-bench` (in `mise run test`) covers the parser and
statistics.
