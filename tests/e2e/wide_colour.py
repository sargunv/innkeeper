"""Inspect the non-interlaced 8-bit RGB/RGBA PNGs our desktop captures produce."""

import struct
import sys
import zlib
from pathlib import Path


def rgb_pixels(png):
  if png[:8] != b"\x89PNG\r\n\x1a\n":
    raise ValueError("not a PNG")
  offset = 8
  compressed = bytearray()
  header = None
  while offset < len(png):
    length, kind = struct.unpack_from(">I4s", png, offset)
    data = png[offset + 8 : offset + 8 + length]
    offset += length + 12  # length, type, data, CRC
    if kind == b"IHDR":
      header = struct.unpack(">IIBBBBB", data)
    elif kind == b"IDAT":
      compressed.extend(data)
    elif kind == b"IEND":
      break
  if header is None:
    raise ValueError("missing PNG header")
  width, height, depth, colour, compression, filtering, interlace = header
  if (
    depth != 8
    or colour not in (2, 6)
    or (compression, filtering, interlace) != (0, 0, 0)
  ):
    raise ValueError("expected non-interlaced 8-bit RGB/RGBA PNG")
  channels = 3 if colour == 2 else 4
  stride = width * channels
  raw = zlib.decompress(compressed)
  if len(raw) != height * (stride + 1):
    raise ValueError("incorrect PNG scanline length")
  previous = bytearray(stride)
  for offset in range(0, len(raw), stride + 1):
    filtering = raw[offset]
    row = bytearray(raw[offset + 1 : offset + 1 + stride])
    if filtering not in range(5):
      raise ValueError("unknown PNG scanline filter")
    for i in range(stride):
      left = row[i - channels] if i >= channels else 0
      above = previous[i]
      upper_left = previous[i - channels] if i >= channels else 0
      if filtering == 1:
        row[i] = (row[i] + left) & 255
      elif filtering == 2:
        row[i] = (row[i] + above) & 255
      elif filtering == 3:
        row[i] = (row[i] + (left + above) // 2) & 255
      elif filtering == 4:
        estimate = left + above - upper_left
        distances = (
          abs(estimate - left),
          abs(estimate - above),
          abs(estimate - upper_left),
        )
        predictor = (left, above, upper_left)[distances.index(min(distances))]
        row[i] = (row[i] + predictor) & 255
    for i in range(0, stride, channels):
      yield tuple(row[i : i + 3])  # Match RGB conversion: alpha is ignored.
    previous = row


def visible(png):
  return (
    sum(
      1
      for r, g, b in rgb_pixels(png)
      if r > 220 and 60 < g < 100 and 10 < b < 50
    )
    > 10000
  )


if __name__ == "__main__":
  sys.exit(0 if visible(Path(sys.argv[1]).read_bytes()) else 1)
