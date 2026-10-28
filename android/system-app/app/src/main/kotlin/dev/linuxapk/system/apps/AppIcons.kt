// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.apps

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
import java.io.ByteArrayOutputStream

/**
 * App icons for the desktop, drawn by Android itself: adaptive icons get the system mask and vector
 * drawables are rasterized by the framework. `linuxapk install` (through [IconDump]) and the
 * bridge's apps service (through [AppMirrorFeature]) get the same pixels.
 */
object AppIcons {
  private const val TAG = "LinuxApkApps"

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

  /** [icon] as a [size]-pixel square PNG. */
  fun png(icon: Drawable, size: Int): ByteArray {
    val bmp = Bitmap.createBitmap(size, size, Bitmap.Config.ARGB_8888)
    // Adaptive icons fill their mask edge to edge; leave a small margin like desktop icon themes
    // do. Legacy icons carry their own padding.
    val pad = if (icon is AdaptiveIconDrawable) size / 24 else 0
    icon.setBounds(pad, pad, size - pad, size - pad)
    icon.draw(Canvas(bmp))
    val out = ByteArrayOutputStream()
    bmp.compress(Bitmap.CompressFormat.PNG, 100, out)
    bmp.recycle()
    return out.toByteArray()
  }
}
