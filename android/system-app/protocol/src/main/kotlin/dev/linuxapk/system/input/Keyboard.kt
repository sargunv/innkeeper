// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.input

import android.view.KeyEvent

/**
 * What Android's key character map types for a key, as `KeyCharacterMap.get(keyCode, metaState)`
 * (0: nothing). Injected keys use `Virtual.kcm`, which is US.
 */
fun interface KeyChars {
  fun get(keyCode: Int, metaState: Int): Int
}

/**
 * Host keyboard state to Android key events and meta state.
 *
 * - Keycodes follow the host layout, as in Mac.apk's host keymap: a key whose level-1 keysym (the
 *   bridge resolves it under the compositor's xkb keymap) is a letter, digit or one of Android's
 *   punctuation keys gets that keycode, so shortcuts follow the layout (Ctrl+Z on AZERTY). Other
 *   keys keep their physical keycode from [EvdevKeymap] (Android's Generic.kl). The scan code is
 *   always the evdev code, so games that read it keep physical positions. Unmapped keys are
 *   dropped.
 * - Text Android would get wrong: when what the key types under the host layout (the line's text,
 *   composed by the bridge from dead keys and Compose) isn't what [chars] gives for the keycode,
 *   the key down carries [OutKey.text] and the Linux.apk Keyboard commits that text instead; a step
 *   of a compose sequence carries empty text and types nothing.
 * - Window shortcuts ([windowShortcuts]): Ctrl+W and Ctrl+Q call [onClose] instead of reaching the
 *   app, and Alt+Left is BACK. Ctrl+W reaches apps that [keepCtrlW] (web browsers close a tab).
 * - Meta state is tracked from the modifier keys themselves, like Android's KeyboardInputMapper (a
 *   modifier's own DOWN already carries its bit, its UP no longer does). Modifiers the compositor
 *   reports as held without us having seen the key (held before the window got focus, or sticky
 *   keys) come from the xkb masks. Caps Lock and Num Lock come from the xkb locked mask.
 * - Key repeat: Android's InputDispatcher synthesizes repeats for injected key downs (they are
 *   trusted), with the host's rate and delay (the system app sets them). A compositor that repeats
 *   keys itself (wl_keyboard v10, after a rate of 0) sends repeats, which become downs with a
 *   repeat count, or taps for a key that is no longer held.
 */
class Keyboard(
  private val escapeAsBack: Boolean = false,
  private val chars: KeyChars = KeyChars { _, _ -> 0 },
  private val windowShortcuts: Boolean = false,
  private val keepCtrlW: () -> Boolean = { false },
  private val escapeBack: () -> Boolean = { escapeAsBack },
  private val onClose: () -> Unit = {},
) {
  /** [clearMeta]: meta bits the key's events leave out (Alt, for Alt+Left as BACK). */
  private class Held(val keyCode: Int, val downTime: Long, val clearMeta: Int = 0) {
    var repeats = 0
  }

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

  fun key(e: HostEvent.Key, time: Long, out: MutableList<OutEvent>) {
    val evdev = e.code
    // Volume belongs to the desktop, even when its compositor passes these keys to us.
    if (evdev in 113..115) return // KEY_MUTE, KEY_VOLUMEDOWN, KEY_VOLUMEUP
    if (!e.pressed) {
      val h = held.remove(evdev) ?: return
      if (h.keyCode == SWALLOWED) return
      updateMeta(h.keyCode)
      out +=
        OutKey(
          KeyEvent.ACTION_UP,
          h.keyCode,
          evdev,
          metaState and h.clearMeta.inv(),
          FLAGS,
          h.downTime,
          time,
        )
      return
    }
    if (e.repeat) {
      val h = held[evdev]
      if (h == null) {
        // KWin repeats a key whose press a host input method passed on as a press and release:
        // each repeat is another tap, as a native toolkit types it.
        key(e.copy(repeat = false), time, out)
        key(e.copy(pressed = false, repeat = false), time, out)
        return
      }
      if (h.keyCode == SWALLOWED) return
      out +=
        OutKey(
          KeyEvent.ACTION_DOWN,
          h.keyCode,
          evdev,
          metaState and h.clearMeta.inv(),
          FLAGS,
          h.downTime,
          time,
          repeatCount = ++h.repeats,
        )
      return
    }
    if (held.containsKey(evdev)) return // a duplicate press
    var code = keycodeFor(evdev, e.sym)
    if (code == KeyEvent.KEYCODE_UNKNOWN) return
    var clearMeta = 0
    if (windowShortcuts) {
      val chord = metaState and CHORD_MASK
      val closes = code == KeyEvent.KEYCODE_Q || (code == KeyEvent.KEYCODE_W && !keepCtrlW())
      if (closes && chord == CTRL_MASK_ON) {
        held[evdev] = Held(SWALLOWED, time)
        onClose()
        return
      }
      if (code == KeyEvent.KEYCODE_DPAD_LEFT && chord == ALT_MASK_ON) {
        code = KeyEvent.KEYCODE_BACK
        clearMeta = ALT_MASK
      }
    }
    held[evdev] = Held(code, time, clearMeta)
    updateMeta(code)
    val meta = metaState and clearMeta.inv()
    val text = if (clearMeta == 0) textFor(code, meta, e) else null
    out += OutKey(KeyEvent.ACTION_DOWN, code, evdev, meta, FLAGS, time, time, text = text)
  }

  // What the IME commits instead of the key's character; null: the key types what it should.
  private fun textFor(code: Int, meta: Int, e: HostEvent.Key): String? {
    // Ctrl and Meta chords are shortcuts, never text.
    if (meta and (KeyEvent.META_CTRL_ON or KeyEvent.META_META_ON) != 0) return null
    if (e.compose) return ""
    val t = e.text ?: return null
    fun types(c: Int) = c != 0 && String(Character.toChars(c)) == t
    // Without Alt too: Alt+F types "f" under xkb but nothing in Android (an Alt chord), while AltGr
    // characters differ from the key's own.
    if (types(chars.get(code, meta)) || types(chars.get(code, meta and ALT_MASK.inv()))) {
      return null
    }
    return t
  }

  /** Focus left the window: release every held key as cancelled. */
  fun releaseAll(time: Long, out: MutableList<OutEvent>) {
    keyMeta = 0
    xkbDepressed = 0
    xkbStale = 0
    for ((evdev, h) in held) {
      if (h.keyCode == SWALLOWED) continue
      out +=
        OutKey(
          KeyEvent.ACTION_UP,
          h.keyCode,
          evdev,
          metaState and h.clearMeta.inv(),
          FLAGS or KeyEvent.FLAG_CANCELED,
          h.downTime,
          time,
        )
    }
    held.clear()
  }

  private fun keycodeFor(evdev: Int, sym: Int): Int {
    val physical = EvdevKeymap.keycode(evdev)
    // Only character keys follow the layout; Escape or F1 stay put whatever a layout puts there.
    val code = if (physical in CHARACTER_KEYS) layoutKeycode(sym) ?: physical else physical
    return if (escapeBack() && code == KeyEvent.KEYCODE_ESCAPE) KeyEvent.KEYCODE_BACK else code
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

  companion object {
    private const val FLAGS = KeyEvent.FLAG_FROM_SYSTEM

    // A key whose events Android never sees (a window shortcut).
    private const val SWALLOWED = -1

    private const val ALT_MASK =
      KeyEvent.META_ALT_ON or KeyEvent.META_ALT_LEFT_ON or KeyEvent.META_ALT_RIGHT_ON
    private const val CHORD_MASK =
      KeyEvent.META_SHIFT_ON or
        KeyEvent.META_CTRL_ON or
        KeyEvent.META_ALT_ON or
        KeyEvent.META_META_ON
    private const val CTRL_MASK_ON = KeyEvent.META_CTRL_ON
    private const val ALT_MASK_ON = KeyEvent.META_ALT_ON

    /** Android's character keys, by the ASCII character of their unshifted US keysym. */
    private val PUNCTUATION =
      mapOf(
        '`' to KeyEvent.KEYCODE_GRAVE,
        '-' to KeyEvent.KEYCODE_MINUS,
        '=' to KeyEvent.KEYCODE_EQUALS,
        '[' to KeyEvent.KEYCODE_LEFT_BRACKET,
        ']' to KeyEvent.KEYCODE_RIGHT_BRACKET,
        '\\' to KeyEvent.KEYCODE_BACKSLASH,
        ';' to KeyEvent.KEYCODE_SEMICOLON,
        '\'' to KeyEvent.KEYCODE_APOSTROPHE,
        ',' to KeyEvent.KEYCODE_COMMA,
        '.' to KeyEvent.KEYCODE_PERIOD,
        '/' to KeyEvent.KEYCODE_SLASH,
        ' ' to KeyEvent.KEYCODE_SPACE,
        '@' to KeyEvent.KEYCODE_AT,
        '#' to KeyEvent.KEYCODE_POUND,
        '*' to KeyEvent.KEYCODE_STAR,
        '+' to KeyEvent.KEYCODE_PLUS,
      )

    private val CHARACTER_KEYS: Set<Int> =
      (KeyEvent.KEYCODE_A..KeyEvent.KEYCODE_Z).toSet() +
        (KeyEvent.KEYCODE_0..KeyEvent.KEYCODE_9) +
        PUNCTUATION.values

    /**
     * The Android keycode for an xkb keysym that is a Latin letter, a digit or one of Android's
     * punctuation keys (their keysyms are their ASCII codes); null for any other.
     */
    fun layoutKeycode(sym: Int): Int? =
      when (sym) {
        in 'a'.code..'z'.code -> KeyEvent.KEYCODE_A + sym - 'a'.code
        in 'A'.code..'Z'.code -> KeyEvent.KEYCODE_A + sym - 'A'.code
        in '0'.code..'9'.code -> KeyEvent.KEYCODE_0 + sym - '0'.code
        in 0x20..0x7e -> PUNCTUATION[sym.toChar()]
        else -> null
      }

    // xkb modifier mask bits in the standard (evdev/pc) keymaps.
    private const val XKB_SHIFT = 1 shl 0
    private const val XKB_LOCK = 1 shl 1
    private const val XKB_CONTROL = 1 shl 2
    private const val XKB_MOD1 = 1 shl 3 // Alt
    private const val XKB_NUM = 1 shl 4 // Mod2 = Num Lock
    private const val XKB_MOD4 = 1 shl 6 // Super

    private val SHIFT = Family(KeyEvent.META_SHIFT_ON, KeyEvent.META_SHIFT_LEFT_ON)
    private val CTRL = Family(KeyEvent.META_CTRL_ON, KeyEvent.META_CTRL_LEFT_ON)
    private val ALT = Family(KeyEvent.META_ALT_ON, KeyEvent.META_ALT_LEFT_ON)
    private val META = Family(KeyEvent.META_META_ON, KeyEvent.META_META_LEFT_ON)
    private val SYM = Family(KeyEvent.META_SYM_ON, 0)
    private val FUNCTION = Family(KeyEvent.META_FUNCTION_ON, 0)

    private val XKB_FALLBACK =
      listOf(XKB_SHIFT to SHIFT, XKB_CONTROL to CTRL, XKB_MOD1 to ALT, XKB_MOD4 to META)

    private val MODIFIER_KEYS: Map<Int, Pair<Family, Int>> =
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
