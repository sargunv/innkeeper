# Manual measurement harness

`mise run bench -- input|frame [--noisy] [--seconds N] [--max-load N]` measures
input and frame latency. It is separate from `mise run test` and `test:e2e`.
Before a run, build and stage everything, including `build:system-app`, and
prepare the image.

Each run creates a fresh `bench-<mode>-<time>` profile on its own nested
desktop. It applies the development memory caps and runs the safety selftest. It
removes the profile and the desktop when it finishes. The harness refuses to run
when the one-minute load average exceeds `--max-load` or another session is
running. `--max-load` defaults to half the logical CPUs. `--noisy` runs anyway
and marks the result as noisy. To measure a session's memory, use
`tools/dev/footprint.sh`.

| Mode    | Measures                                                                                                                    |
| ------- | --------------------------------------------------------------------------------------------------------------------------- |
| `input` | key press to commit and to `wp_presentation` of a probe's colour change, from the bridge trace (`INNKEEPER_BRIDGE_DEBUG=1`) |
| `frame` | commit and presentation intervals of a scrolling list and 200 moving sprites, `--seconds` each (default 5)                  |

Results go to `out/bench/<profile>/`. The directory holds `result.json`,
`summary.txt` and bounded raw traces. A null statistic means there was no
observation. It does not mean zero. Results are not committed or published, and
measurements don't go in docs. Compare runs only when they share the release
build, hardware, compositor and power settings. Nested and software compositors
can show that the harness works, but their numbers are not useful. On a virtual
desktop, presentation times are estimates of scanout and not measurements of the
screen.

To get a baseline from another runtime the user already has, such as the Android
Emulator or Waydroid, install the probes APK with `adb install -r -g`. Launch
`dev.sargunv.innkeeper.probes/.BenchProbe` with `--es mode input|scroll|game`,
and read the results with `adb logcat -d -t 2000 -s InnkeeperBench:I`. Never
install root services, binderfs or kernel modules for a baseline.

`cargo test -p innkeeper-bench` covers the parser and the statistics. It runs as
part of `mise run test`.
