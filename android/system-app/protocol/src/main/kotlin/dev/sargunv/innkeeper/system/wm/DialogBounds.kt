// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.wm

/**
 * The part of a display that a translucent task shows, such as a dialog-themed activity or the
 * dialog that a translucent activity opens (docs/display.md, "Dialogs"). The window shows only this
 * part.
 */
object DialogBounds {
  data class Rect(val left: Int, val top: Int, val right: Int, val bottom: Int) {
    val width
      get() = right - left

    val height
      get() = bottom - top
  }

  /**
   * The union of the app windows in [frames] that leave part of the [width]×[height] display
   * uncovered, clipped to the display. A translucent activity's own window fills the display, so it
   * doesn't count. Null if no such window shows.
   */
  fun crop(width: Int, height: Int, frames: List<Rect>): Rect? {
    var union: Rect? = null
    for (f in frames) {
      val c =
        Rect(
          f.left.coerceIn(0, width),
          f.top.coerceIn(0, height),
          f.right.coerceIn(0, width),
          f.bottom.coerceIn(0, height),
        )
      if (c.width <= 0 || c.height <= 0) continue
      if (c.width == width && c.height == height) continue
      union =
        union?.let {
          Rect(
            minOf(it.left, c.left),
            minOf(it.top, c.top),
            maxOf(it.right, c.right),
            maxOf(it.bottom, c.bottom),
          )
        } ?: c
    }
    return union?.takeUnless { it.width == width && it.height == height }
  }
}
