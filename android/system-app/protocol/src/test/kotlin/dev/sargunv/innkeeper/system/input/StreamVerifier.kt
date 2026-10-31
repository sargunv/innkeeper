// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.input

import android.view.InputDevice
import android.view.MotionEvent

/**
 * A port of the rules in AOSP's InputVerifier (frameworks/native/libs/input/rust/
 * input_verifier.rs) that InputDispatcher applies to injected motion events. All injected events
 * share one device id, so this models a single device. [check] throws on the first violation.
 */
class StreamVerifier {
  private var touching: MutableSet<Int>? = null
  private var hovering: MutableSet<Int>? = null
  private var buttonState = 0
  private var pendingButtons = 0
  private var downTime = 0L

  fun check(e: OutMotion) {
    require(e.source and InputDevice.SOURCE_CLASS_POINTER != 0) { "not a pointer source: $e" }
    val masked = e.action and MotionEvent.ACTION_MASK
    val index = e.action shr MotionEvent.ACTION_POINTER_INDEX_SHIFT
    val ids = e.pointers.map { it.id }
    require(ids.isNotEmpty()) { "no pointers: $e" }
    when (masked) {
      MotionEvent.ACTION_DOWN,
      MotionEvent.ACTION_HOVER_ENTER,
      MotionEvent.ACTION_HOVER_EXIT,
      MotionEvent.ACTION_HOVER_MOVE,
      MotionEvent.ACTION_UP -> require(ids.size == 1) { "needs one pointer: $e" }
      MotionEvent.ACTION_POINTER_DOWN,
      MotionEvent.ACTION_POINTER_UP -> require(index < ids.size) { "bad index: $e" }
      MotionEvent.ACTION_BUTTON_PRESS,
      MotionEvent.ACTION_BUTTON_RELEASE ->
        require(Integer.bitCount(e.actionButton) == 1) { "needs one action button: $e" }
    }
    checkButtons(e, masked)

    val verifyDownTime = masked == MotionEvent.ACTION_DOWN || touching != null
    if (verifyDownTime) require(e.downTime >= downTime) { "down time went backwards: $e" }

    when (masked) {
      MotionEvent.ACTION_DOWN -> {
        require(touching == null) { "DOWN while pointers are down: $e" }
        touching = mutableSetOf(ids[0])
      }
      MotionEvent.ACTION_POINTER_DOWN -> {
        val t = touching ?: error("POINTER_DOWN without DOWN: $e")
        require(t.size == ids.size - 1) { "POINTER_DOWN count mismatch: $e" }
        require(t.add(ids[index])) { "POINTER_DOWN for a down pointer: $e" }
      }
      MotionEvent.ACTION_MOVE -> require(touching == ids.toSet()) { "MOVE pointers: $e" }
      MotionEvent.ACTION_POINTER_UP -> {
        require(touching == ids.toSet()) { "POINTER_UP pointers: $e" }
        touching!!.remove(ids[index])
      }
      MotionEvent.ACTION_UP -> {
        val t = touching ?: error("UP without DOWN: $e")
        require(t.size == 1 && ids[0] in t) { "UP pointers: $e" }
        touching = null
      }
      MotionEvent.ACTION_CANCEL -> {
        require(touching == ids.toSet()) { "CANCEL pointers: $e" }
        touching = null
      }
      MotionEvent.ACTION_HOVER_ENTER -> {
        require(touching == null && hovering == null) { "bad HOVER_ENTER: $e" }
        hovering = mutableSetOf(ids[0])
      }
      MotionEvent.ACTION_HOVER_MOVE -> {
        require(touching == null) { "HOVER_MOVE while pointers are down: $e" }
        (hovering ?: mutableSetOf<Int>().also { hovering = it }).add(ids[0])
      }
      MotionEvent.ACTION_HOVER_EXIT -> {
        val h = hovering ?: error("HOVER_EXIT while not hovering: $e")
        h.remove(ids[0])
        require(h.isEmpty()) { "pointers still hovering: $e" }
        hovering = null
      }
    }
    if (verifyDownTime) downTime = e.downTime
  }

  val idle: Boolean
    get() = touching == null && hovering == null

  private fun checkButtons(e: OutMotion, masked: Int) {
    if (pendingButtons != 0) {
      require(masked == MotionEvent.ACTION_BUTTON_PRESS && pendingButtons and e.actionButton != 0) {
        "expected BUTTON_PRESS after DOWN: $e"
      }
      pendingButtons = pendingButtons and e.actionButton.inv()
    }
    val expected =
      when (masked) {
        MotionEvent.ACTION_DOWN -> {
          require(buttonState and e.buttonState.inv() == 0) { "DOWN missing buttons: $e" }
          pendingButtons = e.buttonState and buttonState.inv()
          e.buttonState
        }
        MotionEvent.ACTION_BUTTON_PRESS -> {
          require(buttonState and e.actionButton == 0) { "duplicate BUTTON_PRESS: $e" }
          buttonState or e.actionButton
        }
        MotionEvent.ACTION_BUTTON_RELEASE -> {
          require(buttonState and e.actionButton != 0) { "BUTTON_RELEASE of unpressed: $e" }
          buttonState and e.actionButton.inv()
        }
        else -> buttonState
      }
    require(e.buttonState == expected) { "button state $expected expected: $e" }
    if (masked != MotionEvent.ACTION_DOWN) buttonState = e.buttonState
  }
}
