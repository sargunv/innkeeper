// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.wm

/**
 * Display 0's mode in phone mode, in its natural orientation: its size in pixels and its density.
 * It follows its window as [WindowMetrics] does for a virtual display.
 */
data class DeviceScreen(val width: Int, val height: Int, val dpi: Int) {
  /** Its shorter side in dp. */
  val smallestWidthDp: Int
    get() = minOf(width, height) * BASE_DPI / dpi

  /** Whether Android lays it out as a large screen, with a taskbar. */
  val large: Boolean
    get() = smallestWidthDp >= LARGE_DP

  /** Its value of `vendor.innkeeper.hwc.primary`, which the composer reads. */
  val size: String
    get() = "${width}x$height"

  override fun toString() = "$size@$dpi"

  companion object {
    private const val BASE_DPI = 160
    private const val LARGE_DP = 600
    /** The composer's largest side, in pixels. */
    const val MAX_SIDE = 8192
    /** The window's smallest side, in dp, which is Android's smallest screen. */
    const val MIN_DP = 320

    /**
     * The screen for a window of [widthDp] by [heightDp] at [scale], which shows display 0 turned
     * by [rotation] quarter turns. A window turned a quarter turn shows the display's height as its
     * width.
     */
    fun of(widthDp: Int, heightDp: Int, scale: Double, rotation: Int): DeviceScreen? {
      val m = WindowMetrics.from(widthDp, heightDp, scale) ?: return null
      val (w, h) = if (rotation % 2 != 0) m.height to m.width else m.width to m.height
      if (w > MAX_SIDE || h > MAX_SIDE) return null
      return DeviceScreen(w, h, m.dpi)
    }

    /** Parses [toString]'s form, or returns null. */
    fun parse(s: String?): DeviceScreen? {
      val m =
        Regex("""(\d{1,5})x(\d{1,5})@(\d{1,4})""").matchEntire(s ?: return null) ?: return null
      val (w, h, dpi) = m.destructured.toList().map(String::toInt)
      if (w !in 1..MAX_SIDE || h !in 1..MAX_SIDE || dpi !in 1..1280) return null
      return DeviceScreen(w, h, dpi)
    }
  }
}
