# Manual measurement harness

`mise run bench -- input|frame` measures input and frame latency. It is
deliberately separate from `mise run test` and `test:e2e`. Parser/statistics
unit tests run with `cargo test -p innkeeper-bench` (also in the workspace unit
suite). Build and stage first, including `build:system-app`, and prepare the
image as for development. For a session's memory, use `tools/dev/footprint.sh`.

Each invocation creates a fresh `bench-<name>-<time>` profile and Android data
directory, runs the existing isolated nested desktop, applies the development
session's memory caps and safety selftest, then stops its session and desktop.
An explicit `INNKEEPER_PROFILE=bench-...` must be unused. Android
window/transition/animator scales are disabled only in this disposable data
directory to exclude launch transitions. The runner, desktop and Android session
are capped separately. No main session or host integration is changed.

The runner prints CPU, kernel, load average, CPU frequency governors, compositor
and other Innkeeper broker PIDs before starting. It refuses when one-minute load
exceeds `--max-load` (default half the available logical CPUs, minimum 1.0; 16.0
on a 32-CPU machine) or another session is present. An explicit `--max-load N`
overrides that default. `--noisy` permits harness proof on a busy machine and
always sets `noisy: true`. It checks load again after collection. Always inspect
context, renderer, refresh rate, display size and raw traces before comparing
runs. A quiet load average alone does not establish a valid experiment. Use the
same release build, hardware, compositor, power settings and workloads for
comparisons; debug builds and nested/software compositors are proof only.

```sh
mise run bench -- input --noisy
mise run bench -- frame --noisy --seconds 2
```

Artifacts stay in `out/bench/<profile>/`: `result.json`, `summary.txt`, and
bounded raw traces. Failed collections retain an error and return nonzero. Null
statistics mean no observation, never zero latency. Nothing is committed or
published automatically. Measurements are not recorded in source documentation.

## Measurements

- **Input:** SPACE on the private Wayland seat toggles a solid red/blue probe.
  Ten presses are serialized by actual frame feedback.
  `INNKEEPER_BRIDGE_DEBUG=1` enables bridge timestamps at input dispatch and
  immediately before surface commit, a centre-pixel read from that dma-buf, and
  `wp_presentation` feedback. The parser matches changed colours to their commit
  ids and presentation ids; painting the old colour cannot count. Callback-only
  visibility commits can supersede content feedback while retaining the same
  pixels. Bench mode observes their presentation too; the parser attributes it
  to the content on that window and counts only its earliest presentation. A
  discard alone never counts as presentation. It subtracts presentation
  timestamps only when the compositor advertises `CLOCK_MONOTONIC`, shared with
  Android's `System.nanoTime`. Raw feedback includes refresh and presentation
  flags. The bridge trace stops after 20,000 records. Reading one pixel and
  logging add overhead; use the same instrumentation for every comparison.
  Compositor presentation is a scanout estimate. Physical input-to-photon
  requires a photodiode or a high-speed camera and a visible colour transition.
  A virtual desktop cannot measure photons.
- **Frame:** the probe animates a text list (`scroll`) and 200 moving sprites
  (`game`) for `--seconds` each (default 5, maximum 60). This is a repeatable
  game-like workload, not a commercial game score. Report commit and
  presentation intervals separately, discarded feedback, nearest-rank p50, p95,
  p99, min/mean/max and millisecond histogram bins `[lo, hi)`. Missing
  presentation support leaves that histogram null; a commit is not a displayed
  frame. Presentations are sorted by timestamp because callbacks can arrive out
  of order.

## Manual baselines

The harness does not provision the Android emulator or Waydroid. For an already
installed emulator, use the same AOSP/API level, x86_64 architecture,
resolution, refresh rate and GPU mode; record its version, command, CPU/memory
allocation and acceleration. Install our probes APK with `adb install -r -g`,
launch `dev.sargunv.innkeeper.probes/.BenchProbe` with
`--es mode input|scroll|game`, and collect bounded
`adb logcat -d -t 2000 -s InnkeeperBench:I`. There is no Innkeeper bridge trace
in an emulator: measure input-to-photon with the same external
camera/photodiode, and use its presentation/Perfetto tools for frame timing.

For a native GTK baseline, make an equivalent drawing-area app with SPACE
changing a solid colour, the same text/sprite workload, window size and refresh
rate. Timestamp its key handler, draw/commit and Wayland presentation feedback
in the same monotonic clock; use the same external optical method for physical
latency.

Waydroid is a manual baseline only if the user has already installed and chosen
to run it. Never install root services, binderfs or kernel modules for these
benchmarks. Match the image/workload/display configuration, install the same
probes, launch the selected mode using Waydroid's Android shell. Collect their
tagged logs and account for the whole container process tree. Document any
baseline that cannot expose the same timestamps rather than substituting shell
timing for a missing metric.
