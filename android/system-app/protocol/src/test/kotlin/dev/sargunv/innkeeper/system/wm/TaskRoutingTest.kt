// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.wm

import dev.sargunv.innkeeper.system.wm.TaskRouting.Destination
import dev.sargunv.innkeeper.system.wm.TaskRouting.Window
import kotlin.test.Test
import kotlin.test.assertEquals

class TaskRoutingTest {
  @Test
  fun launcherFollowsTheTaskWhenItsBasePackageChanges() {
    val windows = listOf(Window(4, 31, "app.A"), Window(5, null, "app.C"))
    assertEquals(4, TaskRouting.launcherWindow(31, "app.B", windows))
    assertEquals(null, TaskRouting.launcherWindow(null, "app.B", windows))
    assertEquals(5, TaskRouting.launcherWindow(null, "app.C", windows))
  }

  @Test
  fun launcherClaimsItsPendingWindowAndAnotherTaskGetsANewOne() {
    assertEquals(Destination.Existing(4), TaskRouting.destination(31, 4, listOf(Window(4, null))))
    val windows = listOf(Window(4, 31))
    assertEquals(Destination.NewWindow, TaskRouting.destination(32, 4, windows))
    // Package is intentionally absent: two tasks of the same app are independent.
    assertEquals(
      Destination.Existing(5),
      TaskRouting.destination(32, 5, windows + Window(5, 32)),
    )
  }

  @Test
  fun crossAppActivityStaysInTaskButNewTaskLeavesCallersWindow() {
    val windows = listOf(Window(4, 31), Window(5, 32))
    // No NEW_TASK: Android changes the top activity, keeping task 31.
    assertEquals(Destination.Existing(4), TaskRouting.destination(31, 4, windows))
    // NEW_TASK/MULTIPLE_TASK: Android supplies a new task id, even on the caller's display.
    assertEquals(Destination.NewWindow, TaskRouting.destination(33, 4, windows))
  }

  @Test
  fun displayZeroGetsANewWindowOrRestoresTheTasksOwnWindow() {
    val windows = listOf(Window(4, 31), Window(5, null))
    assertEquals(Destination.NewWindow, TaskRouting.destination(32, 0, windows))
    assertEquals(Destination.Existing(4), TaskRouting.destination(31, 0, windows))
    assertEquals(Destination.Ignore, TaskRouting.destination(33, 9, windows))
    // A closing window leaves the active registry, but its display is still held (#151).
    assertEquals(Destination.Ignore, TaskRouting.destination(31, 4, emptyList()))
  }
}
