// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.wm

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull

class WindowMetricsTest {
  @Test
  fun movingBetweenIntegerAndFractionalScalesPreservesLogicalSize() {
    val logical = 640 to 480
    assertEquals(
      WindowMetrics(640, 480, 160),
      WindowMetrics.from(logical.first, logical.second, 1.0),
    )
    assertEquals(
      WindowMetrics(1280, 960, 320),
      WindowMetrics.from(logical.first, logical.second, 2.0),
    )
    assertEquals(
      WindowMetrics(960, 720, 240),
      WindowMetrics.from(logical.first, logical.second, 1.5),
    )
    assertEquals(
      WindowMetrics(640, 480, 160),
      WindowMetrics.from(logical.first, logical.second, 1.0),
    )
    // Independent windows retain their own scale; fractional rounding is deterministic.
    assertEquals(WindowMetrics(801, 601, 200), WindowMetrics.from(641, 481, 1.25))
    assertEquals(WindowMetrics(1, 1, 80), WindowMetrics.from(1, 1, 0.5))
  }

  @Test
  fun malformedGeometryDoesNotReachVirtualDisplayResize() {
    for (scale in listOf(Double.NaN, Double.POSITIVE_INFINITY, 0.0, -1.0, 8.1)) assertNull(
      WindowMetrics.from(640, 480, scale),
    )
    assertNull(WindowMetrics.from(0, 480, 1.0))
    assertNull(WindowMetrics.from(640, -1, 1.0))
  }
}
