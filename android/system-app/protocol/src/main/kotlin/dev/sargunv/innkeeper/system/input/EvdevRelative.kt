// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.input

/** Unaccelerated relative motion, retaining subpixel motion across evdev's integer reports. */
class EvdevRelative {
  private var x = 0f
  private var y = 0f

  fun reset() {
    x = 0f
    y = 0f
  }

  fun move(dx: Float, dy: Float): List<Triple<Int, Int, Int>> {
    if (!dx.isFinite() || !dy.isFinite()) return emptyList()
    x += dx
    y += dy
    val ix = x.toInt()
    val iy = y.toInt()
    x -= ix
    y -= iy
    return buildList {
      if (ix != 0) add(Triple(2, 0, ix)) // EV_REL, REL_X
      if (iy != 0) add(Triple(2, 1, iy)) // EV_REL, REL_Y. The bridge appends SYN_REPORT.
    }
  }
}
