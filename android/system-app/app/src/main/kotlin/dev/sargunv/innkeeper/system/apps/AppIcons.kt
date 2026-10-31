// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.apps

import android.content.Intent
import android.content.pm.ActivityInfo
import android.content.pm.ApplicationInfo
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.drawable.AdaptiveIconDrawable
import android.graphics.drawable.Drawable
import android.util.DisplayMetrics
import android.util.Log
import android.util.TypedValue
import dev.sargunv.innkeeper.system.bridge.Wire
import dev.sargunv.innkeeper.system.bridge.toRgba
import java.io.ByteArrayOutputStream

/**
 * App icons for the desktop, drawn by Android itself: adaptive icons get the system mask and vector
 * drawables are rasterized by the framework. The bridge's apps service (through [AppMirrorFeature])
 * get the same pixels.
 */
object AppIcons {
  private const val TAG = "InnkeeperApps"

  /** The package's first launcher activity, if it has one. */
  fun launcherActivity(pm: PackageManager, pkg: String): ActivityInfo? {
    val main = Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER).setPackage(pkg)
    return pm
      .queryIntentActivities(main, PackageManager.ResolveInfoFlags.of(0))
      .firstOrNull()
      ?.activityInfo
  }

  /**
   * The launcher activity's icon (an activity-alias may override the app's), or else the app's, at
   * xxxhdpi so the large sizes stay sharp.
   */
  fun load(pm: PackageManager, app: ApplicationInfo, launcher: ActivityInfo?): Drawable {
    val res = launcher?.iconResource ?: app.icon
    if (res != 0) {
      try {
        pm
          .getResourcesForApplication(app)
          .getDrawableForDensity(res, DisplayMetrics.DENSITY_XXXHIGH, null)
          ?.let {
            return it
          }
      } catch (e: Exception) {
        Log.w(TAG, "${app.packageName}: icon at xxxhdpi: $e")
      }
    }
    return launcher?.loadIcon(pm) ?: app.loadIcon(pm)
  }

  private fun bitmap(icon: Drawable, size: Int): Bitmap {
    val bmp = Bitmap.createBitmap(size, size, Bitmap.Config.ARGB_8888)
    // Adaptive icons fill their mask edge to edge; leave a small margin like desktop icon themes
    // do. Legacy icons carry their own padding.
    val pad = if (icon is AdaptiveIconDrawable) size / 24 else 0
    icon.setBounds(pad, pad, size - pad, size - pad)
    icon.draw(Canvas(bmp))
    return bmp
  }

  /** The same masked launcher pixels as desktop entries, for a task window. */
  fun rgba(icon: Drawable, size: Int): Wire.Rgba? {
    val bmp = bitmap(icon, size)
    return try {
      bmp.toRgba(size)
    } finally {
      bmp.recycle()
    }
  }

  /** [icon] as a [size]-pixel square PNG. */
  fun png(icon: Drawable, size: Int): ByteArray {
    val bmp = bitmap(icon, size)
    val out = ByteArrayOutputStream()
    bmp.compress(Bitmap.CompressFormat.PNG, 100, out)
    bmp.recycle()
    return out.toByteArray()
  }

  /**
   * The colour the app's launch window starts with: `windowSplashScreenBackground`, else its window
   * background if that is a colour, else `colorBackground`. Transparent ones (the framework's
   * default splash background, trampoline activities) don't count.
   */
  fun splashColor(pm: PackageManager, app: ApplicationInfo, a: ActivityInfo?): Int? =
    try {
      val theme = pm.getResourcesForApplication(app).newTheme()
      val res = a?.themeResource?.takeIf { it != 0 } ?: app.theme
      if (res != 0) theme.applyStyle(res, true)
      val v = TypedValue()
      listOf(
          android.R.attr.windowSplashScreenBackground,
          android.R.attr.windowBackground,
          android.R.attr.colorBackground,
        )
        .firstNotNullOfOrNull { attr ->
          v.takeIf {
              theme.resolveAttribute(attr, it, true) &&
                it.type in TypedValue.TYPE_FIRST_COLOR_INT..TypedValue.TYPE_LAST_COLOR_INT &&
                it.data ushr 24 != 0
            }
            ?.data
        }
    } catch (e: Exception) {
      Log.w(TAG, "splash colour of ${app.packageName}: $e")
      null
    }
}
