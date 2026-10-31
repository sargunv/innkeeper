// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.wm

import kotlin.math.roundToInt

/** Pixels and density change together, preserving the host window's logical content size. */
data class WindowMetrics(val width: Int, val height: Int, val dpi: Int) {
  companion object {
    fun from(widthDp: Int, heightDp: Int, scale: Double): WindowMetrics? {
      if (widthDp <= 0 || heightDp <= 0 || scale !in 0.5..8.0) return null
      return WindowMetrics(
        (widthDp * scale).roundToInt().coerceAtLeast(1),
        (heightDp * scale).roundToInt().coerceAtLeast(1),
        (160 * scale).roundToInt(),
      )
    }
  }
}
