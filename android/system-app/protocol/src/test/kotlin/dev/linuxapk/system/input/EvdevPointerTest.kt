// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.input

import android.view.KeyEvent
import android.view.MotionEvent
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertTrue

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

  @Test
  fun inheritedControlClickKeepsInjectionThroughRelease() {
    val p = EvdevPointer()
    assertFalse(p.native(MotionEvent.ACTION_DOWN, 1, KeyEvent.META_CTRL_ON, 0))
    assertFalse(p.native(MotionEvent.ACTION_BUTTON_PRESS, 1, KeyEvent.META_CTRL_ON, 0))
    assertFalse(p.native(MotionEvent.ACTION_BUTTON_RELEASE, 0, 0, 0))
    assertFalse(p.native(MotionEvent.ACTION_UP, 0, 0, 0))
  }

  @Test
  fun releasingShiftThenLeavingEndsTheInjectedHover() {
    val p = EvdevPointer()
    val hover =
      OutMotion(
        source = 0x2002,
        action = MotionEvent.ACTION_HOVER_MOVE,
        actionButton = 0,
        buttonState = 0,
        metaState = KeyEvent.META_SHIFT_ON,
        downTime = 0,
        eventTime = 10,
        pointers = listOf(OutPointer(0, MotionEvent.TOOL_TYPE_MOUSE, 20f, 30f, 0f)),
        displayId = 7,
      )
    assertFalse(p.native(hover.action, 0, hover.metaState, 0))
    p.injected(hover)
    assertTrue(p.native(MotionEvent.ACTION_HOVER_EXIT, 0, 0, 0))
    assertEquals(
      hover.copy(action = MotionEvent.ACTION_HOVER_EXIT, eventTime = 20),
      p.exitInjected(20),
    )
    assertNull(p.exitInjected(21))
  }
}
