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

  /** What a window shows next: nothing yet, or [Show.crop] of its display (null: all of it). */
  sealed interface Shape {
    object Wait : Shape

    data class Show(val crop: Rect?) : Shape
  }

  /**
   * One window's choice between a dialog and its whole display. A translucent task whose first
   * drawn windows leave part of the display uncovered is a dialog. Once the window shows its whole
   * display, it never crops again, so the popups and dialogs of a translucent app with its own
   * full-display UI, such as one that swipes back to the activity behind it, don't shrink it.
   */
  class Latch {
    private var whole = false

    /**
     * The shape for a task that is [translucent] or not, with the app [frames] of its
     * [width]×[height] display. A translucent task waits until it draws a window.
     */
    fun next(translucent: Boolean, width: Int, height: Int, frames: List<Rect>): Shape {
      if (translucent && !whole && frames.isEmpty()) return Shape.Wait
      val crop = if (translucent && !whole) crop(width, height, frames) else null
      if (crop == null) whole = true
      return Shape.Show(crop)
    }
  }
}
