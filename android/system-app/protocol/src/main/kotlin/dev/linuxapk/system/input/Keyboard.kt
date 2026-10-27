// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.input

import android.view.KeyEvent

/**
 * Host keyboard state to Android key events and meta state.
 *
 * - Keycodes come from [EvdevKeymap] (Android's Generic.kl). Unmapped keys are dropped.
 * - Meta state is tracked from the modifier keys themselves, like Android's KeyboardInputMapper (a
 *   modifier's own DOWN already carries its bit, its UP no longer does). Modifiers the compositor
 *   reports as held without us having seen the key (held before the window got focus, or sticky
 *   keys) come from the xkb masks. Caps Lock and Num Lock come from the xkb locked mask.
 * - Key repeat: Wayland never sends repeats; Android's InputDispatcher synthesizes them for
 *   injected key downs (they are trusted), with Android's repeat timeout and delay.
 */
class Keyboard(private val escapeAsBack: Boolean = false) {
  private class Held(val keyCode: Int, val downTime: Long)

  private val held = LinkedHashMap<Int, Held>()
  private var keyMeta = 0
  private var xkbDepressed = 0
  private var xkbLocked = 0
  // Modifier families whose key changed since the last `modifiers` line: the xkb masks are stale
  // for them until the compositor sends the update that follows the key event.
  private var xkbStale = 0

  /** Current Android meta state (for key and motion events). */
  val metaState: Int
    get() {
      var m = keyMeta
      for ((xkbBit, family) in XKB_FALLBACK) {
        if (xkbDepressed and xkbBit != 0 && (m or xkbStale) and family.on == 0) {
          m = m or family.on or family.left
        }
      }
      if (xkbLocked and XKB_LOCK != 0) m = m or KeyEvent.META_CAPS_LOCK_ON
      if (xkbLocked and XKB_NUM != 0) m = m or KeyEvent.META_NUM_LOCK_ON
      return m
    }

  fun modifiers(depressed: Int, latched: Int, locked: Int) {
    xkbDepressed = depressed or latched
    xkbLocked = locked
    xkbStale = 0
  }

  fun key(evdev: Int, pressed: Boolean, time: Long, out: MutableList<OutEvent>) {
    if (pressed) {
      if (held.containsKey(evdev)) return // no repeats in the stream; ignore duplicates
      val code = keycodeFor(evdev)
      if (code == KeyEvent.KEYCODE_UNKNOWN) return
      held[evdev] = Held(code, time)
      updateMeta(code)
      out += OutKey(KeyEvent.ACTION_DOWN, code, evdev, metaState, FLAGS, time, time)
    } else {
      val h = held.remove(evdev) ?: return
      updateMeta(h.keyCode)
      out += OutKey(KeyEvent.ACTION_UP, h.keyCode, evdev, metaState, FLAGS, h.downTime, time)
    }
  }

  /** Focus left the window: release every held key as cancelled. */
  fun releaseAll(time: Long, out: MutableList<OutEvent>) {
    keyMeta = 0
    xkbDepressed = 0
    xkbStale = 0
    for ((evdev, h) in held) {
      out +=
        OutKey(
          KeyEvent.ACTION_UP,
          h.keyCode,
          evdev,
          metaState,
          FLAGS or KeyEvent.FLAG_CANCELED,
          h.downTime,
          time,
        )
    }
    held.clear()
  }

  private fun keycodeFor(evdev: Int): Int {
    val code = EvdevKeymap.keycode(evdev)
    return if (escapeAsBack && code == KeyEvent.KEYCODE_ESCAPE) KeyEvent.KEYCODE_BACK else code
  }

  // Called after [held] changed. Recomputes from the keys still held, so releasing one Shift
  // keeps the other's bits.
  private fun updateMeta(keyCode: Int) {
    val (family, _) = MODIFIER_KEYS[keyCode] ?: return
    xkbStale = xkbStale or family.on
    var m = 0
    for (h in held.values) {
      val (f, side) = MODIFIER_KEYS[h.keyCode] ?: continue
      m = m or f.on or side
    }
    keyMeta = m
  }

  private class Family(val on: Int, val left: Int)

  private companion object {
    const val FLAGS = KeyEvent.FLAG_FROM_SYSTEM

    // xkb modifier mask bits in the standard (evdev/pc) keymaps.
    const val XKB_SHIFT = 1 shl 0
    const val XKB_LOCK = 1 shl 1
    const val XKB_CONTROL = 1 shl 2
    const val XKB_MOD1 = 1 shl 3 // Alt
    const val XKB_NUM = 1 shl 4 // Mod2 = Num Lock
    const val XKB_MOD4 = 1 shl 6 // Super

    val SHIFT = Family(KeyEvent.META_SHIFT_ON, KeyEvent.META_SHIFT_LEFT_ON)
    val CTRL = Family(KeyEvent.META_CTRL_ON, KeyEvent.META_CTRL_LEFT_ON)
    val ALT = Family(KeyEvent.META_ALT_ON, KeyEvent.META_ALT_LEFT_ON)
    val META = Family(KeyEvent.META_META_ON, KeyEvent.META_META_LEFT_ON)
    val SYM = Family(KeyEvent.META_SYM_ON, 0)
    val FUNCTION = Family(KeyEvent.META_FUNCTION_ON, 0)

    val XKB_FALLBACK =
      listOf(XKB_SHIFT to SHIFT, XKB_CONTROL to CTRL, XKB_MOD1 to ALT, XKB_MOD4 to META)

    val MODIFIER_KEYS: Map<Int, Pair<Family, Int>> =
      mapOf(
        KeyEvent.KEYCODE_SHIFT_LEFT to (SHIFT to KeyEvent.META_SHIFT_LEFT_ON),
        KeyEvent.KEYCODE_SHIFT_RIGHT to (SHIFT to KeyEvent.META_SHIFT_RIGHT_ON),
        KeyEvent.KEYCODE_CTRL_LEFT to (CTRL to KeyEvent.META_CTRL_LEFT_ON),
        KeyEvent.KEYCODE_CTRL_RIGHT to (CTRL to KeyEvent.META_CTRL_RIGHT_ON),
        KeyEvent.KEYCODE_ALT_LEFT to (ALT to KeyEvent.META_ALT_LEFT_ON),
        KeyEvent.KEYCODE_ALT_RIGHT to (ALT to KeyEvent.META_ALT_RIGHT_ON),
        KeyEvent.KEYCODE_META_LEFT to (META to KeyEvent.META_META_LEFT_ON),
        KeyEvent.KEYCODE_META_RIGHT to (META to KeyEvent.META_META_RIGHT_ON),
        KeyEvent.KEYCODE_SYM to (SYM to 0),
        KeyEvent.KEYCODE_FUNCTION to (FUNCTION to 0),
      )
  }
}
