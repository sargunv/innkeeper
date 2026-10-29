// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.input

import kotlin.test.Test
import kotlin.test.assertEquals

class EvdevPointerTest {
  @Test
  fun outsideDragReturnsToTheHostPosition() {
    val p = EvdevPointer()
    assertEquals(EvdevPointer.Step(-20, 0, 0f, 10f), p.move(-10f, 10f, 10f, 10f, 100, 100))
    assertEquals(EvdevPointer.Step(10, 0, 10f, 10f), p.move(10f, 10f, 0f, 10f, 100, 100))
  }

  @Test
  fun resizeResynchronizesWithAndroid() {
    val p = EvdevPointer()
    p.move(90f, 90f, 10f, 10f, 100, 100)
    assertEquals(EvdevPointer.Step(-39, -39, 10f, 10f), p.move(10f, 10f, 49f, 49f, 50, 50))
  }
}
