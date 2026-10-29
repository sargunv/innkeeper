// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.wm

import android.app.ActivityManager

/** Hidden `TaskInfo` fields, read by reflection (open to platform-signed apps). */
object Tasks {
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
