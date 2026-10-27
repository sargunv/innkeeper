// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.wm

import android.content.pm.ActivityInfo
import android.content.res.Resources

/**
 * A new window's size in dp, from the launch activity's metadata (docs/windowing.md, "Window
 * size"):
 * 1. `<layout android:defaultWidth/defaultHeight>` (Android's own freeform sizing hints);
 * 2. else `android:screenOrientation`: portrait-only activities get a phone-sized portrait window,
 *    landscape-only ones a 16:9 landscape window;
 * 3. else a tablet-like 1280x800.
 *
 * `<layout android:minWidth/minHeight>` become the window's minimum size.
 */
data class WindowSizing(
  val widthDp: Int,
  val heightDp: Int,
  val minWidthDp: Int,
  val minHeightDp: Int,
) {
  companion object {
    const val PHONE_W = 412
    const val PHONE_H = 915
    const val WIDE_W = 1280
    const val WIDE_H = 720
    const val TABLET_W = 1280
    const val TABLET_H = 800

    fun of(ai: ActivityInfo?): WindowSizing {
      // <layout> dimensions were parsed to pixels at the system density.
      val density = Resources.getSystem().displayMetrics.density.takeIf { it > 0f } ?: 1f
      fun dp(px: Int) = if (px > 0) (px / density).toInt() else 0
      val layout = ai?.windowLayout
      val minW = dp(layout?.minWidth ?: 0)
      val minH = dp(layout?.minHeight ?: 0)
      val lw = dp(layout?.width ?: 0)
      val lh = dp(layout?.height ?: 0)
      val (w, h) =
        when {
          lw > 0 && lh > 0 -> lw to lh
          ai != null && isPortrait(ai.screenOrientation) -> PHONE_W to PHONE_H
          ai != null && isLandscape(ai.screenOrientation) -> WIDE_W to WIDE_H
          else -> TABLET_W to TABLET_H
        }
      return WindowSizing(maxOf(w, minW), maxOf(h, minH), minW, minH)
    }

    private fun isPortrait(o: Int) =
      o == ActivityInfo.SCREEN_ORIENTATION_PORTRAIT ||
        o == ActivityInfo.SCREEN_ORIENTATION_SENSOR_PORTRAIT ||
        o == ActivityInfo.SCREEN_ORIENTATION_REVERSE_PORTRAIT ||
        o == ActivityInfo.SCREEN_ORIENTATION_USER_PORTRAIT

    private fun isLandscape(o: Int) =
      o == ActivityInfo.SCREEN_ORIENTATION_LANDSCAPE ||
        o == ActivityInfo.SCREEN_ORIENTATION_SENSOR_LANDSCAPE ||
        o == ActivityInfo.SCREEN_ORIENTATION_REVERSE_LANDSCAPE ||
        o == ActivityInfo.SCREEN_ORIENTATION_USER_LANDSCAPE
  }
}
