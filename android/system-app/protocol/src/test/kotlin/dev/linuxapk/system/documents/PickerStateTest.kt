// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.documents

import kotlin.test.Test
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class PickerStateTest {
  @Test
  fun coldConnectionStartsOnlyOnePickAndDeliversOnlyOneResult() {
    val state = PickerState()
    assertTrue(state.connected())
    // The immediate check and HELLO callback can both report readiness.
    assertFalse(state.connected())
    assertFalse(state.done)
    assertTrue(state.finish())
    assertFalse(state.finish())
    assertFalse(state.connected())
  }

  @Test
  fun timeoutCancellationAndTeardownIgnoreLateCallbacks() {
    for (connected in listOf(false, true)) {
      val state = PickerState()
      if (connected) assertTrue(state.connected())
      // Timeout/fallback before connection, or cancel/destroy with PICK in flight.
      assertTrue(state.finish())
      assertTrue(state.done)
      assertFalse(state.connected())
      assertFalse(state.finish())
    }
  }
}
