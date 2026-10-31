// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.wm

import android.content.pm.ActivityInfo

/** Android's manifest modes that accept NEW_TASK | MULTIPLE_TASK (without NEW_DOCUMENT). */
object LaunchPolicy {
  fun multipleTasks(launchMode: Int, documentMode: Int): Boolean =
    when (launchMode) {
      ActivityInfo.LAUNCH_MULTIPLE,
      ActivityInfo.LAUNCH_SINGLE_TOP -> documentMode != ActivityInfo.DOCUMENT_LAUNCH_NEVER
      ActivityInfo.LAUNCH_SINGLE_INSTANCE_PER_TASK -> true
      else -> false
    }
}
