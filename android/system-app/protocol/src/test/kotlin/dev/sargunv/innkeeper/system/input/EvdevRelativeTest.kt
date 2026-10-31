// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.input

import kotlin.test.Test
import kotlin.test.assertEquals

class EvdevRelativeTest {
  @Test
  fun fractionsAccumulateAndSignsAndAxesSurvive() {
    val p = EvdevRelative()
    assertEquals(emptyList(), p.move(.75f, -.25f))
    assertEquals(listOf(Triple(2, 0, 1), Triple(2, 1, -2)), p.move(.75f, -2f))
    assertEquals(listOf(Triple(2, 0, -3), Triple(2, 1, 1)), p.move(-3.5f, 1.25f))
    p.move(.5f, .5f)
    p.reset()
    assertEquals(emptyList(), p.move(.5f, .5f))
  }
}
