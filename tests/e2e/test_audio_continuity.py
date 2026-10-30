"""The private-source proof must reject dropped samples and inserted silence."""

import math
import unittest

from audio.continuity import analyze


class ContinuityTest(unittest.TestCase):
  def setUp(self):
    self.tone = [
      round(10000 * math.sin(i * 2 * math.pi * 440 / 48000))
      for i in range(48000)
    ]

  def test_continuous_tone(self):
    self.assertTrue(analyze(self.tone)["pass"])

  def test_missing_sample(self):
    samples = self.tone[:16000] + self.tone[16001:]
    self.assertFalse(analyze(samples)["pass"])

  def test_transient_between_crossings(self):
    samples = self.tone.copy()
    samples[12020] += 2000
    self.assertFalse(analyze(samples)["pass"])

  def test_silence_inserted_by_hal(self):
    samples = self.tone[:16000] + [0] * 4096 + self.tone[20096:]
    self.assertFalse(analyze(samples)["pass"])
