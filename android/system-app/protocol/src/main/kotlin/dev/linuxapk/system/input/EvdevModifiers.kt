// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.input

import android.view.KeyEvent

/** InputReader's keyboard meta state, including modifiers inherited through the host xkb masks. */
class EvdevModifiers {
  private var held = emptySet<Int>()
  private var locks = 0

  /** Commit only after the reliable evdev transport accepts the batch. */
  fun sync(meta: Int, send: (List<Triple<Int, Int, Int>>) -> Boolean): Boolean {
    val next = linkedSetOf<Int>()
    for ((on, left, right, lcode, rcode) in families) {
      if (meta and left != 0 || (meta and on != 0 && meta and (left or right) == 0)) next += lcode
      if (meta and right != 0) next += rcode
    }
    if (meta and KeyEvent.META_FUNCTION_ON != 0) next += 464
    val events = ArrayList<Triple<Int, Int, Int>>()
    for (code in held - next) events += Triple(1, code, 0)
    for (code in next - held) events += Triple(1, code, 1)
    for ((mask, code) in lockKeys) {
      if ((locks xor meta) and mask != 0) {
        events += Triple(1, code, 1)
        events += Triple(1, code, 0)
      }
    }
    if (events.isNotEmpty() && !send(events)) return false
    held = next
    locks = meta and (KeyEvent.META_CAPS_LOCK_ON or KeyEvent.META_NUM_LOCK_ON)
    return true
  }

  private data class Family(
    val on: Int,
    val left: Int,
    val right: Int,
    val lcode: Int,
    val rcode: Int,
  )

  companion object {
    private val families =
      listOf(
        Family(
          KeyEvent.META_SHIFT_ON,
          KeyEvent.META_SHIFT_LEFT_ON,
          KeyEvent.META_SHIFT_RIGHT_ON,
          42,
          54,
        ),
        Family(
          KeyEvent.META_CTRL_ON,
          KeyEvent.META_CTRL_LEFT_ON,
          KeyEvent.META_CTRL_RIGHT_ON,
          29,
          97,
        ),
        Family(
          KeyEvent.META_ALT_ON,
          KeyEvent.META_ALT_LEFT_ON,
          KeyEvent.META_ALT_RIGHT_ON,
          56,
          100,
        ),
        Family(
          KeyEvent.META_META_ON,
          KeyEvent.META_META_LEFT_ON,
          KeyEvent.META_META_RIGHT_ON,
          125,
          126,
        ),
      )
    private val lockKeys = listOf(KeyEvent.META_CAPS_LOCK_ON to 58, KeyEvent.META_NUM_LOCK_ON to 69)

    // Only this path produces app modifier events; ordinary keys stay injected.
    fun owns(keyCode: Int): Boolean = keyCode in modifierKeys

    private val modifierKeys =
      setOf(
        KeyEvent.KEYCODE_SHIFT_LEFT,
        KeyEvent.KEYCODE_SHIFT_RIGHT,
        KeyEvent.KEYCODE_CTRL_LEFT,
        KeyEvent.KEYCODE_CTRL_RIGHT,
        KeyEvent.KEYCODE_ALT_LEFT,
        KeyEvent.KEYCODE_ALT_RIGHT,
        KeyEvent.KEYCODE_META_LEFT,
        KeyEvent.KEYCODE_META_RIGHT,
        KeyEvent.KEYCODE_FUNCTION,
        KeyEvent.KEYCODE_CAPS_LOCK,
        KeyEvent.KEYCODE_NUM_LOCK,
      )
  }
}
