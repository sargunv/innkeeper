// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.wm

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull

class TaskIconTest {
  @Test
  fun taskIconThenLauncherIconThenNone() {
    assertEquals("task", TaskIcon.choose({ "task" }, { error("task icon takes priority") }))
    assertEquals("launcher", TaskIcon.choose({ null }, { "launcher" }))
    assertNull(TaskIcon.choose<String>({ null }, { null }))
  }
}
