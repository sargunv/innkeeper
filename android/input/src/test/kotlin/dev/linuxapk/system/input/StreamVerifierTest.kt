// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.input

import android.view.InputDevice
import android.view.MotionEvent
import kotlin.test.Test
import kotlin.test.assertFails

// The verifier model must reject what AOSP's InputVerifier rejects, or the other tests prove
// nothing.
class StreamVerifierTest {
  private fun ev(
    source: Int,
    action: Int,
    button: Int = 0,
    state: Int = 0,
    ids: List<Int> = listOf(0),
  ) = OutMotion(source, action, button, state, 0, 0, 0, ids.map { OutPointer(it, 0, 0f, 0f, 0f) })

  private val mouse = InputDevice.SOURCE_MOUSE
  private val touch = InputDevice.SOURCE_TOUCHSCREEN

  @Test
  fun rejectsHoverDuringTouch() {
    val v = StreamVerifier()
    v.check(ev(touch, MotionEvent.ACTION_DOWN))
    assertFails { v.check(ev(mouse, MotionEvent.ACTION_HOVER_MOVE)) }
  }

  @Test
  fun rejectsDownWithoutButtonPress() {
    val v = StreamVerifier()
    v.check(ev(mouse, MotionEvent.ACTION_DOWN, state = MotionEvent.BUTTON_PRIMARY))
    assertFails {
      v.check(ev(mouse, MotionEvent.ACTION_MOVE, state = MotionEvent.BUTTON_PRIMARY))
    }
  }

  @Test
  fun rejectsUpBeforeButtonRelease() {
    val v = StreamVerifier()
    v.check(ev(mouse, MotionEvent.ACTION_DOWN, state = 1))
    v.check(ev(mouse, MotionEvent.ACTION_BUTTON_PRESS, button = 1, state = 1))
    assertFails { v.check(ev(mouse, MotionEvent.ACTION_UP)) }
  }

  @Test
  fun rejectsPointerDownCountMismatchAndExitWithoutHover() {
    val v = StreamVerifier()
    v.check(ev(touch, MotionEvent.ACTION_DOWN))
    assertFails {
      v.check(ev(touch, MotionEvent.ACTION_POINTER_DOWN or (1 shl 8), ids = listOf(0, 1, 2)))
    }
    assertFails {
      StreamVerifier().check(ev(mouse, MotionEvent.ACTION_HOVER_EXIT))
    }
  }
}
