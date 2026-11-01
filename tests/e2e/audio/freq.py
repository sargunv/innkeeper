#!/usr/bin/env python3
"""Report the dominant frequency and level of a raw capture without numpy.

  freq.py FILE [--rate 48000] [--channels 2] [--format s16|f32] [--expect HZ]

Reads interleaved little-endian PCM, takes channel 0, skips silence at the
start, and runs a Hann-windowed radix-2 FFT over the loudest 2^16-sample
stretch. Prints RMS (dBFS), the peak frequency (parabolic interpolation) and,
with --expect, exits 1 unless the peak is within 2 Hz of HZ and the signal is
not silent.
"""

import argparse
import array
import cmath
import math
import sys


def fft(a):
  n = len(a)
  j = 0
  for i in range(1, n):
    bit = n >> 1
    while j & bit:
      j ^= bit
      bit >>= 1
    j |= bit
    if i < j:
      a[i], a[j] = a[j], a[i]
  size = 2
  while size <= n:
    w = cmath.exp(-2j * math.pi / size)
    half = size // 2
    tw = [w**k for k in range(half)]
    for start in range(0, n, size):
      for k in range(half):
        u = a[start + k]
        v = a[start + k + half] * tw[k]
        a[start + k] = u + v
        a[start + k + half] = u - v
    size *= 2
  return a


def main():
  p = argparse.ArgumentParser()
  p.add_argument("file")
  p.add_argument("--rate", type=int, default=48000)
  p.add_argument("--channels", type=int, default=2)
  p.add_argument("--format", choices=["s16", "f32"], default="s16")
  p.add_argument("--expect", type=float)
  p.add_argument("--min-dbfs", type=float, default=-60.0)
  a = p.parse_args()
  raw = open(a.file, "rb").read()
  if a.format == "s16":
    x = array.array("h")
    x.frombytes(raw[: len(raw) // 2 * 2])
    scale = 32768.0
  else:
    x = array.array("f")
    x.frombytes(raw[: len(raw) // 4 * 4])
    scale = 1.0
  if sys.byteorder != "little":
    x.byteswap()
  ch0 = [v / scale for v in x[:: a.channels]]
  if not ch0:
    print("empty capture")
    return 1
  rms_all = math.sqrt(sum(v * v for v in ch0) / len(ch0))
  n = 1 << 16
  while n > len(ch0) and n > 1024:
    n >>= 1
  # Find the loudest window, stepping by 1/8 of a window.
  best, best_e = 0, -1.0
  for s in range(0, max(1, len(ch0) - n + 1), n // 8):
    e = sum(v * v for v in ch0[s : s + n : 16])
    if e > best_e:
      best, best_e = s, e
  seg = ch0[best : best + n]
  if len(seg) < n:
    seg += [0.0] * (n - len(seg))
  rms = math.sqrt(sum(v * v for v in seg) / n)
  db = 20 * math.log10(rms) if rms > 0 else -math.inf
  win = [
    seg[i] * (0.5 - 0.5 * math.cos(2 * math.pi * i / (n - 1))) for i in range(n)
  ]
  spec = fft([complex(v, 0) for v in win])
  mags = [abs(c) for c in spec[: n // 2]]
  k = max(range(1, n // 2 - 1), key=lambda i: mags[i])
  y0, y1, y2 = (math.log(mags[i] + 1e-12) for i in (k - 1, k, k + 1))
  d = 0.5 * (y0 - y2) / (y0 - 2 * y1 + y2) if (y0 - 2 * y1 + y2) != 0 else 0
  f = (k + d) * a.rate / n
  print(
    f"samples {len(ch0)} ({len(ch0) / a.rate:.2f} s), window {n} at {best / a.rate:.2f} s"
  )
  print(
    f"level: window {db:.1f} dBFS, whole capture {20 * math.log10(rms_all) if rms_all > 0 else -math.inf:.1f} dBFS"
  )
  print(f"dominant frequency: {f:.2f} Hz")
  if a.expect is not None:
    ok = abs(f - a.expect) <= 2.0 and db > a.min_dbfs
    print(
      "PASS" if ok else "FAIL",
      f"(expected {a.expect} Hz, above {a.min_dbfs} dBFS)",
    )
    return 0 if ok else 1
  return 0 if db > a.min_dbfs else 1


if __name__ == "__main__":
  sys.exit(main())
