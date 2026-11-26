// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.wm

import android.content.Intent
import android.content.pm.ActivityInfo

object LaunchPolicy {
  /** Android's manifest modes that accept NEW_TASK | MULTIPLE_TASK (without NEW_DOCUMENT). */
  fun multipleTasks(launchMode: Int, documentMode: Int): Boolean =
    when (launchMode) {
      ActivityInfo.LAUNCH_MULTIPLE,
      ActivityInfo.LAUNCH_SINGLE_TOP -> documentMode != ActivityInfo.DOCUMENT_LAUNCH_NEVER
      ActivityInfo.LAUNCH_SINGLE_INSTANCE_PER_TASK -> true
      else -> false
    }

  /**
   * NEW_TASK | RESET_TASK_IF_NEEDED, as a launcher uses, brings the app's existing task to the
   * front. A requested activity other than the launcher's goes without the reset, which drops it
   * when that task's root is another activity, and opens on top of the task as a shortcut's
   * activity does.
   */
  fun launchFlags(requested: Boolean): Int =
    if (requested) Intent.FLAG_ACTIVITY_NEW_TASK
    else Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_RESET_TASK_IF_NEEDED

  /** What a launch that opens a window does with the app's existing task. */
  enum class TaskUse {
    START,
    MOVE,
    MOVE_AND_START,
  }

  /**
   * A launch that opens a window takes the app's existing task, such as one started on display 0 or
   * left on a closing window's display, unless it asks for a new task or hands over a link or file.
   * A requested activity other than the launcher's then starts in that task, as it does in an
   * existing window.
   */
  fun taskUse(hasTask: Boolean, newTask: Boolean, handoff: Boolean, requested: Boolean): TaskUse =
    when {
      !hasTask || newTask || handoff -> TaskUse.START
      requested -> TaskUse.MOVE_AND_START
      else -> TaskUse.MOVE
    }
}
