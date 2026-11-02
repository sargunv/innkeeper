"""Check whether a desktop screenshot shows a color.

  colour.py <png> <r> <g> <b>

It succeeds when more than 10000 pixels are within 20 of the color in each
channel.
"""

import sys

import gi

gi.require_version("GdkPixbuf", "2.0")
from gi.repository import GdkPixbuf  # noqa: E402


def visible(path, want):
  shot = GdkPixbuf.Pixbuf.new_from_file(path)
  pixels = shot.get_pixels()
  stride, step = shot.get_rowstride(), shot.get_n_channels()
  count = 0
  for y in range(shot.get_height()):
    row = pixels[y * stride : y * stride + shot.get_width() * step]
    for i in range(0, len(row), step):
      if all(abs(row[i + c] - want[c]) < 20 for c in range(3)):
        count += 1
  return count > 10000


if __name__ == "__main__":
  sys.exit(0 if visible(sys.argv[1], [int(v) for v in sys.argv[2:5]]) else 1)
