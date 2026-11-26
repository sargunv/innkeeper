// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.wm

import android.content.Intent
import android.content.pm.ActivityInfo
import dev.sargunv.innkeeper.system.wm.LaunchPolicy.TaskUse
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class LaunchPolicyTest {
  @Test
  fun newWindowRespectsLauncherTaskAndDocumentModes() {
    for (mode in listOf(ActivityInfo.LAUNCH_MULTIPLE, ActivityInfo.LAUNCH_SINGLE_TOP)) {
      assertTrue(LaunchPolicy.multipleTasks(mode, ActivityInfo.DOCUMENT_LAUNCH_NONE))
      assertTrue(LaunchPolicy.multipleTasks(mode, ActivityInfo.DOCUMENT_LAUNCH_ALWAYS))
      assertTrue(LaunchPolicy.multipleTasks(mode, ActivityInfo.DOCUMENT_LAUNCH_INTO_EXISTING))
      assertFalse(LaunchPolicy.multipleTasks(mode, ActivityInfo.DOCUMENT_LAUNCH_NEVER))
    }
    assertTrue(
      LaunchPolicy.multipleTasks(
        ActivityInfo.LAUNCH_SINGLE_INSTANCE_PER_TASK,
        ActivityInfo.DOCUMENT_LAUNCH_NEVER,
      ),
    )
    for (mode in listOf(ActivityInfo.LAUNCH_SINGLE_TASK, ActivityInfo.LAUNCH_SINGLE_INSTANCE)) {
      assertFalse(LaunchPolicy.multipleTasks(mode, ActivityInfo.DOCUMENT_LAUNCH_NONE))
      assertFalse(LaunchPolicy.multipleTasks(mode, ActivityInfo.DOCUMENT_LAUNCH_ALWAYS))
    }
  }

  @Test
  fun aNewWindowTakesTheAppsTaskAndStartsTheRequestedActivityInIt() {
    assertEquals(TaskUse.MOVE, LaunchPolicy.taskUse(true, false, false, false))
    // A probe launched by component after the app's task was left outside a window.
    assertEquals(TaskUse.MOVE_AND_START, LaunchPolicy.taskUse(true, false, false, true))
    assertEquals(TaskUse.START, LaunchPolicy.taskUse(false, false, false, true))
    assertEquals(TaskUse.START, LaunchPolicy.taskUse(true, true, false, true))
    assertEquals(TaskUse.START, LaunchPolicy.taskUse(true, false, true, true))
  }

  @Test
  fun onlyALauncherLaunchResetsTheAppsTask() {
    val reset = Intent.FLAG_ACTIVITY_RESET_TASK_IF_NEEDED
    assertEquals(reset, LaunchPolicy.launchFlags(requested = false) and reset)
    assertEquals(0, LaunchPolicy.launchFlags(requested = true) and reset)
    for (requested in listOf(false, true)) {
      val flags = LaunchPolicy.launchFlags(requested)
      assertEquals(Intent.FLAG_ACTIVITY_NEW_TASK, flags and Intent.FLAG_ACTIVITY_NEW_TASK)
    }
  }
}
