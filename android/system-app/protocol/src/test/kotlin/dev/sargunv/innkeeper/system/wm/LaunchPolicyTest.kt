// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.wm

import android.content.pm.ActivityInfo
import kotlin.test.Test
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
}
