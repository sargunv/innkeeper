// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.input

import kotlin.test.Test
import kotlin.test.assertEquals

class EvdevRemovalsTest {
  @Test
  fun unacknowledgedRemovalSurvivesBridgeRestartWithoutReplayingAcknowledgedOnes() {
    val removals = EvdevRemovals()
    val attempts = ArrayList<Int>()
    removals.add(3)
    removals.add(8)
    // A window that closes during disconnection cannot have its removal acknowledged.
    removals.flush { device ->
      attempts += device
      device == 8
    }
    assertEquals(listOf(3, 8), attempts)
    // Reconnect retries the lost request. Successful removals are not repeated.
    removals.flush { device ->
      attempts += device
      true
    }
    assertEquals(listOf(3, 8, 3), attempts)
    removals.flush { device ->
      attempts += device
      true
    }
    assertEquals(listOf(3, 8, 3), attempts)
  }
}
