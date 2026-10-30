// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.wm

import dev.linuxapk.system.input.HostEvent
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull

class WindowVisibilityTest {
  @Test
  fun hideRestoreAndDuplicates() {
    assertEquals(HostEvent.WindowVisible(false, 7), HostEvent.parse("window_visible 0 7"))
    assertEquals(HostEvent.WindowVisible(true, 7), HostEvent.parse("window_visible 1 7"))
    assertNull(HostEvent.parse("window_visible 2 7"))
    val state = WindowVisibility()
    assertNull(state.update(true))
    assertEquals(false, state.update(false))
    assertNull(state.update(false))
    assertEquals(true, state.update(true))
    assertNull(state.update(true))
  }
}
