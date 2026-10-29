// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.wm

import android.app.ActivityManager
import android.content.Context
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.drawable.Icon
import dev.linuxapk.system.bridge.Wire
import dev.linuxapk.system.bridge.toRgba

/** Hidden `TaskInfo` fields, read by reflection (open to platform-signed apps). */
object Tasks {
  private val loadIcon by lazy { ActivityManager.TaskDescription::class.java.getMethod("loadIcon") }

  /** Includes resource icons and icons persisted by system_server, not just in-memory bitmaps. */
  fun descriptionIcon(context: Context, description: ActivityManager.TaskDescription): Wire.Rgba? {
    val icon = (loadIcon.invoke(description) as? Icon)?.loadDrawable(context) ?: return null
    val width = icon.intrinsicWidth.coerceAtLeast(1)
    val height = icon.intrinsicHeight.coerceAtLeast(1)
    val scale = minOf(1f, 256f / maxOf(width, height))
    val w = (width * scale).toInt().coerceAtLeast(1)
    val h = (height * scale).toInt().coerceAtLeast(1)
    val bitmap = Bitmap.createBitmap(w, h, Bitmap.Config.ARGB_8888)
    return try {
      icon.setBounds(0, 0, w, h)
      icon.draw(Canvas(bitmap))
      bitmap.toRgba(256)
    } finally {
      bitmap.recycle()
    }
  }

  private val displayIdField by lazy { android.app.TaskInfo::class.java.getField("displayId") }
  private val activityTypeField by lazy {
    android.app.TaskInfo::class.java.getField("topActivityType")
  }

  private val topActivityInfoField by lazy {
    android.app.TaskInfo::class.java.getField("topActivityInfo")
  }

  fun topActivityInfo(t: ActivityManager.RunningTaskInfo): android.content.pm.ActivityInfo? =
    topActivityInfoField.get(t) as? android.content.pm.ActivityInfo

  // WindowConfiguration.ACTIVITY_TYPE_*
  private const val ACTIVITY_TYPE_UNDEFINED = 0
  private const val ACTIVITY_TYPE_STANDARD = 1

  fun displayId(t: ActivityManager.RunningTaskInfo): Int = displayIdField.getInt(t)

  /** An ordinary app task: not home, recents, the assistant or a dream. */
  fun isAppTask(t: ActivityManager.RunningTaskInfo): Boolean {
    val type = activityTypeField.getInt(t)
    return (type == ACTIVITY_TYPE_STANDARD || type == ACTIVITY_TYPE_UNDEFINED) &&
      (t.baseActivity ?: t.topActivity) != null
  }
}
