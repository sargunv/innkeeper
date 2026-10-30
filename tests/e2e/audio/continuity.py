#!/usr/bin/env python3
"""Check sustained s16 mono capture of a private 440 Hz sine, without numpy."""

import argparse
import array
import json
import math
import sys


def analyze(samples, rate=48000, frequency=440):
  """Require tone in every 100 ms window and uninterrupted zero-crossing phase."""
  crossing = []
  for i in range(1, len(samples)):
    if samples[i - 1] < 0 <= samples[i]:
      crossing.append(i - 1 - samples[i - 1] / (samples[i] - samples[i - 1]))
  periods = [b - a for a, b in zip(crossing, crossing[1:])]
  expected = rate / frequency
  # One missing/duplicated sample changes a crossing interval by ~1 sample.
  # s16 quantization changes it by less than 0.01 samples at the test level.
  errors = [abs(p - expected) for p in periods]
  window = rate // 10
  levels = []
  frequencies = []
  at = 0
  for start in range(0, len(samples) - window + 1, window):
    part = samples[start : start + window]
    levels.append(math.sqrt(sum(x * x for x in part) / window) / 32768)
    while at < len(crossing) and crossing[at] < start:
      at += 1
    end = at
    while end < len(crossing) and crossing[end] < start + window:
      end += 1
    frequencies.append(
      (end - at - 1) * rate / (crossing[end - 1] - crossing[at])
      if end - at >= 2
      else 0.0
    )
  bad_periods = sum(e > 0.2 for e in errors)
  peak = max((abs(x) for x in samples), default=0)
  coefficient = 2 * math.cos(2 * math.pi * frequency / rate)
  # A continuous sine satisfies this recurrence. It also catches a transient
  # between crossings, which crossing intervals alone cannot detect.
  residual = max(
    (
      abs(samples[i] - coefficient * samples[i - 1] + samples[i - 2])
      for i in range(2, len(samples))
    ),
    default=0,
  )
  normalized_residual = residual / peak if peak else 0
  ok = (
    bool(levels)
    and min(levels) > 0.01
    and bool(errors)
    and bad_periods == 0
    and normalized_residual < 0.005
    and all(abs(f - frequency) < 0.1 for f in frequencies)
  )
  return {
    "pass": ok,
    "samples": len(samples),
    "seconds": len(samples) / rate,
    "windows": len(levels),
    "min_rms": min(levels, default=0),
    "min_frequency_hz": min(frequencies, default=0),
    "max_frequency_hz": max(frequencies, default=0),
    "bad_crossing_intervals": bad_periods,
    "max_normalized_residual": normalized_residual,
    "max_period_error_samples": max(errors, default=0),
  }


def main():
  p = argparse.ArgumentParser(description=__doc__)
  p.add_argument("file")
  p.add_argument(
    "--skip", type=float, default=1.0, help="initial capture warmup, seconds"
  )
  p.add_argument("--seconds", type=float, default=30.0)
  args = p.parse_args()
  samples = array.array("h")
  with open(args.file, "rb") as f:
    f.seek(round(args.skip * 48000) * 2)
    samples.frombytes(f.read(round(args.seconds * 48000) * 2))
  if sys.byteorder != "little":
    samples.byteswap()
  result = analyze(samples)
  if len(samples) != round(args.seconds * 48000):
    result["pass"] = False
    result["error"] = "short recording"
  print(json.dumps(result, indent=2))
  return 0 if result["pass"] else 1


if __name__ == "__main__":
  sys.exit(main())
