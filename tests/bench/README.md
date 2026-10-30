# Manual measurement harness

`mise run bench -- <name>` runs `input`, `frame`, `audio`, `gpu`, `memory` or
`binder`. It is deliberately separate from `mise run test` and `test:e2e`.
Parser/statistics unit tests run with `cargo test -p linuxapk-bench` (also in
the workspace unit suite). Build and stage first, including `build:system-app`,
and prepare the image as for development. Native audio needs the compiler and
the runtime `libpulse-simple.so.0`, with no development headers or root
installation.

Each invocation creates a fresh `bench-<name>-<time>` instance and Android data
directory, runs the existing isolated nested desktop, applies the development
session's memory caps and safety selftest, then stops its session and desktop.
An explicit `LINUXAPK_INSTANCE=bench-...` must be unused. Android
window/transition/animator scales are disabled only in this disposable data
directory to exclude launch transitions. The runner, desktop and Android session
are capped separately. No main session or host integration is changed. The
private audio sink is removed after collection.

The runner prints CPU, kernel, load average, CPU frequency governors, compositor
and other Linux.apk broker PIDs before starting. It refuses when one-minute load
exceeds `--max-load` (default half the available logical CPUs, minimum 1.0; 16.0
on a 32-CPU machine) or another session is present. An explicit `--max-load N`
overrides that default. `--noisy` permits harness proof on a busy machine and
always sets `noisy: true` and `valid_for_comparison: false`. It checks load
again after collection. Always inspect context, renderer, refresh rate, display
size and raw traces before comparing runs. A quiet load average alone does not
establish a valid experiment. Use the same release build, hardware, compositor,
power settings and workloads for comparisons; debug builds and nested/software
compositors are proof only.

```sh
mise run bench -- input --noisy
mise run bench -- frame --noisy --seconds 2
mise run bench -- audio --noisy
mise run bench -- gpu --noisy
mise run bench -- memory --noisy --seconds 2
mise run bench -- binder --noisy
```

Artifacts stay in `out/bench/<instance>/`: `result.json` (schema version 1),
`summary.txt`, and bounded raw traces. Failed collections retain an error and
return nonzero. Null statistics mean no observation, never zero latency. GPU
components and audio round trips can be explicitly unavailable. Nothing is
committed or published automatically. `LINUXAPK_BENCH_SCREENSHOT=1` optionally
captures the probe window after collection on KWin (requires Spectacle).
`measurement_complete: false` identifies missing components, and always prevents
`valid_for_comparison`. Recorded measurements belong in the repository wiki
after quiet-machine runs, not in source documentation. Issue
[#46](https://github.com/sargunv/linux.apk/issues/46) remains open until those
measurements are recorded.

## Measurements

- **Input:** SPACE on the private Wayland seat toggles a solid red/blue probe.
  Ten presses are serialized by actual frame feedback. `LINUXAPK_BENCH=1`
  enables bridge timestamps at input dispatch and immediately before surface
  commit, a centre-pixel read from that dma-buf, and `wp_presentation` feedback.
  The parser matches changed colours to their commit ids and presentation ids;
  painting the old colour cannot count. It subtracts presentation timestamps
  only when the compositor advertises `CLOCK_MONOTONIC`, shared with Android's
  `System.nanoTime`. Raw feedback includes refresh and presentation flags. The
  bridge trace stops after 20,000 records. Reading one pixel and logging add
  overhead; use the same instrumentation for every comparison. Compositor
  presentation is a scanout estimate. Physical input-to-photon requires a
  photodiode or a high-speed camera and a visible colour transition. A virtual
  desktop cannot measure photons.
- **Frame:** the probe animates a text list (`scroll`) and 200 moving sprites
  (`game`) for `--seconds` each (default 5, maximum 60). This is a repeatable
  game-like workload, not a commercial game score. Report commit and
  presentation intervals separately, discarded feedback, nearest-rank p50, p95,
  p99, min/mean/max and millisecond histogram bins `[lo, hi)`. Missing
  presentation support leaves that histogram null; a commit is not a displayed
  frame. Presentations are sorted by timestamp because callbacks can arrive out
  of order.
- **Audio:** AudioTrack plays a repeatable marker into a private PipeWire null
  sink. Output is the AudioTrack timestamp extrapolated to the first submitted
  sample, including Android and downstream queueing. It is an app-visible
  software estimate, not an acoustic DAC measurement. Concurrent AudioRecord
  capture looks for the same 127-sample pseudo-random marker with a normalized
  matched filter; a detection yields a round-trip upper bound at read-return,
  including capture buffering. Today's input HAL supplies silence: absent
  markers are explicitly unavailable. An acoustic/wired output-to-input route
  and a working Android capture backend are prerequisites for that comparison.
  The native baseline uses libpulse-simple on PipeWire, the same sample rate,
  private sink and marker. It reports a first-sample output estimate
  extrapolated from write completion plus queued output time minus the submitted
  100 ms buffer, the raw queued output latency, and the monitor round-trip upper
  bound. Queue estimates and timestamp estimates have different error sources;
  keep both methods visible in comparisons. Physical tests should use a wired
  loopback with the same route and buffer sizes for both clients.
- **GPU:** provide `LINUXAPK_GLMARK2_APK=/absolute/path/glmark2.apk` and/or
  `LINUXAPK_GLMARK2_HOST=/absolute/path/glmark2-wayland`. Without them the
  result explains what is unavailable; it does not download arbitrary APKs.
  Obtain source from [upstream glmark2](https://github.com/glmark2/glmark2),
  record its commit and build native Wayland/GLES and Android x86_64 variants
  using upstream instructions in a bounded build container or mise toolchain.
  Keep the APK and all build products under `out/`, never in Git. The Android
  package/activity must be `org.linaro.glmark2/.Glmark2Activity`. Upstream's old
  Android wrapper targets SDK 23: provide a build updated to the current SDK,
  with an exported benchmark activity, rather than weakening installation
  policy. Both variants run `build:duration=2` and `texture:duration=2` at
  1280x800; Android receives upstream's `args` intent extra, and the runner
  reads its native `files/last_run.log`. Use the host GLES binary
  (`glmark2-es2-wayland`) to match Android's API. Record renderer, resolution,
  GLES version and benchmark list from the raw logs. GPU deadlines are bounded;
  variants with another activity/log path need manual execution and score
  collection.
- **CPU/memory:** one interval with no probe app and one with the static colour
  probe. Reports session cgroup CPU usage as percent of one core, cgroup charged
  memory, and per-process PSS from `smaps_rollup` (null if procfs denies it).
  Cgroup memory includes cache; PSS apportions shared pages. These include the
  session's bridge and broker, but not the separate nested desktop. This is warm
  idle after boot/install, not a claim of a settled idle baseline.
- **Binder:** a service in a distinct Android process echoes a synchronous
  four-byte transaction. Measures 500 round trips with one worker and 500 per
  worker with eight synchronized workers. Transactions use BinderProxy through
  the real broker, check replies and never time shell launch or service lookup.
  Logs include all per-call durations and elapsed/count throughput. This is a
  small echo workload including Parcel overhead; it is not AOSP's
  `binderThroughputTest`. Use that binary manually with `-w 8` when available,
  recording its version and arguments alongside results.

## Manual baselines

The harness does not provision the Android emulator or Waydroid. For an already
installed emulator, use the same AOSP/API level, x86_64 architecture,
resolution, refresh rate and GPU mode; record its version, command, CPU/memory
allocation and acceleration. Install our probes APK with `adb install -r -g`,
launch `dev.linuxapk.probes/.BenchProbe` with
`--es mode input|scroll|game|audio|binder`, and, for `audio` or `binder`, wait
until the activity is visible and start its workers explicitly:

```sh
adb shell am broadcast -a dev.linuxapk.BENCH_BEGIN -p dev.linuxapk.probes
```

Then collect bounded `adb logcat -d -t 2000 -s LinuxApkBench:I`. Use the same
sampling intervals and host cgroup/procfs accounting for its whole process tree.
There is no Linux.apk bridge trace in an emulator: measure input-to-photon with
the same external camera/photodiode, and use its presentation/Perfetto tools for
frame timing. Run the same glmark2 build and scene list manually.

For a native GTK baseline, make an equivalent drawing-area app with SPACE
changing a solid colour, the same text/sprite workload, window size and refresh
rate. Timestamp its key handler, draw/commit and Wayland presentation feedback
in the same monotonic clock; use the same external optical method for physical
latency. Compare native PipeWire audio with the endpoints described above.

Waydroid is a manual baseline only if the user has already installed and chosen
to run it. Never install root services, binderfs or kernel modules for these
benchmarks. Match the image/workload/display configuration, install the same
probes, launch the selected mode, and send the same `dev.linuxapk.BENCH_BEGIN`
broadcast for audio/binder after its window is visible, using Waydroid's Android
shell. Collect their tagged logs and account for the whole container process
tree. Document any baseline that cannot expose the same timestamps rather than
substituting shell timing for a missing metric.
