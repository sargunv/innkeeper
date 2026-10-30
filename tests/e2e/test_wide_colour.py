"""Decode desktop PNG captures without third-party Python packages."""

import struct
import unittest
import zlib

from wide_colour import rgb_pixels, visible


def chunk(kind, data):
  return (
    struct.pack(">I", len(data))
    + kind
    + data
    + struct.pack(">I", zlib.crc32(kind + data))
  )


def png(width, height, colour, scanlines, depth=8, interlace=0):
  header = struct.pack(
    ">IIBBBBB", width, height, depth, colour, 0, 0, interlace
  )
  data = zlib.compress(scanlines)
  return (
    b"\x89PNG\r\n\x1a\n"
    + chunk(b"IHDR", header)
    + chunk(b"IDAT", data[:3])
    + chunk(b"IDAT", data[3:])
    + chunk(b"IEND", b"")
  )


class WideColourTest(unittest.TestCase):
  def test_all_filters_rgb_and_rgba(self):
    # Fixed filtered bytes exercise left edges and wraparound. Each second
    # scanline decodes to the same known pixels.
    fixtures = [
      (
        2,
        [240, 80, 32, 17, 90, 150],
        [
          [250, 70, 20, 255, 120, 40],
          [250, 70, 20, 5, 50, 20],
          [10, 246, 244, 238, 30, 146],
          [130, 30, 4, 122, 40, 211],
          [10, 246, 244, 238, 40, 146],
        ],
        [(240, 80, 32), (17, 90, 150), (250, 70, 20), (255, 120, 40)],
      ),
      (
        6,
        [240, 80, 32, 17, 90, 150, 200, 23],
        [
          [250, 70, 20, 255, 120, 40, 230, 128],
          [250, 70, 20, 255, 126, 226, 210, 129],
          [10, 246, 244, 238, 30, 146, 30, 105],
          [130, 30, 4, 247, 206, 186, 120, 245],
          [10, 246, 244, 238, 30, 146, 30, 129],
        ],
        [(240, 80, 32), (90, 150, 200), (250, 70, 20), (120, 40, 230)],
      ),
    ]
    for colour, first, filtered, expected in fixtures:
      for filtering, second in enumerate(filtered):
        with self.subTest(colour=colour, filtering=filtering):
          image = png(2, 2, colour, bytes([0] + first + [filtering] + second))
          self.assertEqual(list(rgb_pixels(image)), expected)

  def test_first_row_and_paeth_predictors(self):
    for filtering in range(5):
      image = png(1, 1, 2, bytes([filtering, 240, 80, 32]))
      self.assertEqual(list(rgb_pixels(image)), [(240, 80, 32)])
    # Paeth chooses upper-left for the second pixel's red and green channels.
    image = png(
      2,
      2,
      2,
      bytes(
        [
          0,
          100,
          100,
          100,
          0,
          200,
          100,
          4,
          100,
          156,
          0,
          50,
          176,
          246,
        ]
      ),
    )
    self.assertEqual(
      list(rgb_pixels(image)),
      [
        (100, 100, 100),
        (0, 200, 100),
        (200, 0, 100),
        (150, 20, 90),
      ],
    )
    # Above wins a tie with upper-left (predictors 3 and 1 respectively).
    image = png(
      2,
      2,
      2,
      bytes(
        [
          0,
          1,
          1,
          1,
          3,
          3,
          3,
          4,
          255,
          255,
          255,
          0,
          0,
          0,
        ]
      ),
    )
    self.assertEqual(
      list(rgb_pixels(image)),
      [
        (1, 1, 1),
        (3, 3, 3),
        (0, 0, 0),
        (3, 3, 3),
      ],
    )

  def test_visible_threshold_and_blank(self):
    for colour, alpha in [(2, b""), (6, b"\x00")]:
      for count in (10000, 10001):
        with self.subTest(colour=colour, count=count):
          orange = png(
            count, 1, colour, b"\0" + (bytes([240, 80, 32]) + alpha) * count
          )
          blank = png(count, 1, colour, b"\0" + (b"\0\0\0" + alpha) * count)
          self.assertEqual(visible(orange), count > 10000)
          self.assertFalse(visible(blank))

  def test_rejects_unsupported_layouts(self):
    for options in ({"depth": 16}, {"interlace": 1}, {"colour": 3}):
      with self.subTest(options=options), self.assertRaises(ValueError):
        list(
          rgb_pixels(
            png(1, 1, **({"colour": 2} | options), scanlines=b"\0\0\0\0")
          )
        )

  def test_rejects_bad_scanlines(self):
    for scanline in (b"\0\0", b"\x05\0\0\0"):
      with self.subTest(scanline=scanline), self.assertRaises(ValueError):
        list(rgb_pixels(png(1, 1, 2, scanline)))
