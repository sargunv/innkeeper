// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.input

import android.view.KeyEvent
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull
import kotlin.test.assertTrue

/**
 * The host layout, compose and window shortcuts, with the bridge's `key` lines as it sends them
 * under French (AZERTY), German (QWERTZ) and US keymaps (host/bridge/src/input/xkb.rs tests the
 * bridge's half against real keymaps).
 */
class KeyboardTest {
  private val closed = ArrayList<Int>()
  private val browsers = HashSet<Int>() // displays
  private var now = 10_000L
  private var tr = translator(windowShortcuts = true)

  private fun translator(windowShortcuts: Boolean) =
    InputTranslator(
      TranslatorConfig(keyChars = UsVirtualKcm, windowShortcuts = windowShortcuts),
      now = { now },
      keepsCtrlW = { it in browsers },
      onClose = { closed += it },
    )

  private fun feed(vararg lines: String): List<OutKey> =
    lines.flatMap { tr.handle(requireNotNull(HostEvent.parse(it)) { it }) }.map { it as OutKey }

  private fun down(line: String) = feed(line).single().also { assertEquals(it.action, 0) }

  @Test
  fun mediaKeysReachAndroidButVolumeStaysOnTheHost() {
    feed("keyboard_enter 7")
    for ((scan, code) in
      listOf(164 to KeyEvent.KEYCODE_MEDIA_PLAY_PAUSE, 163 to KeyEvent.KEYCODE_MEDIA_NEXT)) {
      val events = feed("key 1 $scan 1", "key 2 $scan 0")
      assertEquals(listOf(code, code), events.map { it.keyCode })
      assertEquals(listOf(0, 1), events.map { it.action })
      assertTrue(events.all { it.displayId == 7 })
    }
    for (scan in 113..115) {
      assertTrue(feed("key 3 $scan 1", "key 4 $scan 2", "key 5 $scan 0").isEmpty())
    }
    assertTrue(feed("keyboard_leave 7").isEmpty())
  }

  @Test
  fun lettersTakeTheirKeycodeFromTheLayout() {
    // AZERTY: the Q position types 'a', the W position 'z'.
    val a = down("key 1 16 1 97 text a")
    assertEquals(KeyEvent.KEYCODE_A, a.keyCode)
    assertEquals(16, a.scanCode) // games keep the physical position
    assertNull(a.text) // Virtual.kcm types 'a' for KEYCODE_A
    // QWERTZ: the Y position is 'z'.
    assertEquals(KeyEvent.KEYCODE_Z, down("key 2 21 1 122 text z").keyCode)
    // Punctuation Android has a key for: German '+' is KEYCODE_PLUS.
    assertEquals(KeyEvent.KEYCODE_PLUS, down("key 3 27 1 43 text +").keyCode)
  }

  @Test
  fun otherKeysStayPhysical() {
    // AZERTY's é key is KEY_2 and types é: KEYCODE_2, and the IME commits é.
    val e = down("key 1 3 1 233 text é")
    assertEquals(KeyEvent.KEYCODE_2, e.keyCode)
    assertEquals("é", e.text)
    // Cyrillic letters keep the QWERTY position, so Ctrl+C still copies.
    assertEquals(KeyEvent.KEYCODE_C, down("key 2 46 1 1747 text с").keyCode)
    // Non-character keys never move, whatever the layout puts there.
    assertEquals(KeyEvent.KEYCODE_ESCAPE, down("key 3 1 1 97").keyCode)
    // Old lines (no keymap) are physical.
    assertEquals(KeyEvent.KEYCODE_Q, down("key 4 16 1").keyCode)
  }

  @Test
  fun shortcutsFollowTheLayout() {
    // Ctrl+Z on AZERTY: the bridge sends no text for a Ctrl chord.
    feed("key 1 29 1 65507", "modifiers 4 0 0 0")
    val z = down("key 2 17 1 122")
    assertEquals(KeyEvent.KEYCODE_Z, z.keyCode)
    assertTrue(z.metaState and KeyEvent.META_CTRL_ON != 0)
    assertNull(z.text)
    // A Ctrl chord on a key that types a non-ASCII letter is a shortcut too, never text.
    assertNull(down("key 3 3 1 233 text é").text)
  }

  @Test
  fun textVirtualKcmWouldGetWrongIsCommitted() {
    // AZERTY Shift+& is '1': KEYCODE_1 with Shift would be '!'.
    feed("key 1 42 1 65505", "modifiers 1 0 0 0")
    assertEquals("1", down("key 2 2 1 38 text 1").text)
    // Shift+a is 'A' either way.
    assertNull(down("key 3 16 1 97 text A").text)
    feed("key 4 42 0", "modifiers 0 0 0 0")
    // AZERTY ',' is KEYCODE_COMMA; unshifted they agree.
    assertNull(down("key 5 50 1 44 text ,").text)
  }

  @Test
  fun altChordsAreShortcutsButAltGrTypes() {
    // US Alt+F: xkb types 'f', Android Alt+F types nothing: an Alt chord, not text.
    feed("key 1 56 1 65513", "modifiers 8 0 0 0")
    assertNull(down("key 2 33 1 102 text f").text)
    feed("key 3 56 0", "modifiers 0 0 0 0", "key 4 33 0")
    // AZERTY AltGr+e is '€' (AltGr is KEYCODE_ALT_RIGHT, as on an Android device).
    feed("key 5 100 1 65027", "modifiers 128 0 0 0")
    assertEquals("€", down("key 6 18 1 101 text €").text)
  }

  @Test
  fun composeStepsTypeNothingAndTheResultIsCommitted() {
    // AZERTY dead circumflex (on [), then e.
    assertEquals("", down("key 1 26 1 65106 compose").text)
    feed("key 2 26 0")
    val e = down("key 3 18 1 101 text ê")
    assertEquals(KeyEvent.KEYCODE_E, e.keyCode)
    assertEquals("ê", e.text)
    // US with compose:ralt: Multi_key, ~, n.
    assertEquals("", down("key 4 100 1 65312 compose").text)
    feed("key 5 100 0")
    assertEquals("ñ", down("key 6 49 1 110 text ñ").text)
  }

  @Test
  fun ctrlWAndCtrlQCloseTheWindow() {
    feed("keyboard_enter 5", "key 1 29 1 65507", "modifiers 4 0 0 0")
    // AZERTY's W is at the Z position: the layout decides.
    assertTrue(feed("key 2 44 1 119").isEmpty())
    assertEquals(listOf(5), closed)
    assertTrue(feed("key 3 44 0").isEmpty())
    assertTrue(feed("key 4 16 1 113", "key 5 16 0").isEmpty())
    assertEquals(listOf(5, 5), closed)
    // Ctrl+Shift+W is the app's.
    feed("key 6 42 1 65505")
    assertEquals(KeyEvent.KEYCODE_W, feed("key 7 44 1 119").single().keyCode)
  }

  @Test
  fun browsersKeepCtrlW() {
    browsers += 6
    feed("keyboard_enter 6", "key 1 29 1 65507", "modifiers 4 0 0 0")
    assertEquals(KeyEvent.KEYCODE_W, feed("key 2 17 1 119").single().keyCode)
    assertTrue(feed("key 3 17 0", "key 4 16 1 113").none { it.action == KeyEvent.ACTION_DOWN })
    assertEquals(listOf(6), closed)
  }

  @Test
  fun altLeftIsBack() {
    feed("keyboard_enter 5", "key 1 56 1 65513", "modifiers 8 0 0 0")
    val (down, up) = feed("key 2 105 1 65361", "key 3 105 0")
    assertEquals(KeyEvent.KEYCODE_BACK, down.keyCode)
    assertEquals(KeyEvent.KEYCODE_BACK, up.keyCode)
    assertEquals(5, down.displayId)
    // BACK, not Alt+BACK.
    assertEquals(0, down.metaState and KeyEvent.META_ALT_ON)
    assertEquals(0, up.metaState and KeyEvent.META_ALT_ON)
  }

  @Test
  fun windowShortcutsCanBeOff() {
    tr = translator(windowShortcuts = false)
    feed("key 1 29 1 65507", "modifiers 4 0 0 0")
    assertEquals(KeyEvent.KEYCODE_W, feed("key 2 17 1 119").single().keyCode)
    assertTrue(closed.isEmpty())
    feed("key 3 29 0", "modifiers 0 0 0 0", "key 4 56 1 65513", "modifiers 8 0 0 0")
    assertEquals(KeyEvent.KEYCODE_DPAD_LEFT, feed("key 5 105 1 65361").single().keyCode)
  }

  @Test
  fun aSwallowedKeyIsNotReleasedOnFocusLoss() {
    feed("key 1 29 1 65507", "modifiers 4 0 0 0", "key 2 17 1 119")
    val released = feed("keyboard_leave")
    assertEquals(listOf(KeyEvent.KEYCODE_CTRL_LEFT), released.map { it.keyCode })
  }

  @Test
  fun compositorRepeatsAreRepeatedDowns() {
    val first = down("key 1 16 1 97 text a")
    // KWin's repeats carry the press's time; they are stamped when they come.
    now += 300
    val r1 = feed("key 1 16 2").single()
    now += 25
    val r2 = feed("key 1 16 2").single()
    assertEquals(listOf(1, 2), listOf(r1.repeatCount, r2.repeatCount))
    assertEquals(listOf(first.downTime, first.downTime), listOf(r1.downTime, r2.downTime))
    assertTrue(first.eventTime < r1.eventTime && r1.eventTime + 25 == r2.eventTime)
    assertEquals(KeyEvent.KEYCODE_A, r1.keyCode)
    assertEquals(KeyEvent.ACTION_UP, feed("key 4 16 0").single().action)
    // KWin with a host input method: the press came as a press and release, then repeats.
    val (d, u) = feed("key 5 16 2 97 text a")
    assertEquals(listOf(KeyEvent.ACTION_DOWN, KeyEvent.ACTION_UP), listOf(d.action, u.action))
    assertEquals(listOf(0, KeyEvent.KEYCODE_A), listOf(d.repeatCount, d.keyCode))
    assertEquals("é", feed("key 6 3 2 233 text é").first().text)
    // A swallowed shortcut's repeats stay swallowed.
    feed("key 7 29 1 65507", "modifiers 4 0 0 0", "key 8 17 1 119")
    assertTrue(feed("key 9 17 2 119").isEmpty())
  }

  @Test
  fun layoutKeycodes() {
    assertEquals(KeyEvent.KEYCODE_A, Keyboard.layoutKeycode('a'.code))
    assertEquals(KeyEvent.KEYCODE_Z, Keyboard.layoutKeycode('Z'.code))
    assertEquals(KeyEvent.KEYCODE_7, Keyboard.layoutKeycode('7'.code))
    assertEquals(KeyEvent.KEYCODE_SLASH, Keyboard.layoutKeycode('/'.code))
    assertNull(Keyboard.layoutKeycode('&'.code))
    assertNull(Keyboard.layoutKeycode(0xe9)) // é
    assertNull(Keyboard.layoutKeycode(0xff1b)) // Escape
  }
}

/** Android's Virtual.kcm (US) for the keys these tests use. */
object UsVirtualKcm : KeyChars {
  private const val EXACT = KeyEvent.META_CTRL_ON or KeyEvent.META_ALT_ON or KeyEvent.META_META_ON

  private val SHIFTED_DIGITS = ")!@#$%^&*("
  private val PUNCT =
    mapOf(
      KeyEvent.KEYCODE_COMMA to ",<",
      KeyEvent.KEYCODE_PERIOD to ".>",
      KeyEvent.KEYCODE_PLUS to "++",
      KeyEvent.KEYCODE_LEFT_BRACKET to "[{",
      KeyEvent.KEYCODE_SPACE to "  ",
    )

  override fun get(keyCode: Int, metaState: Int): Int {
    if (metaState and EXACT != 0) return 0
    val shift = metaState and KeyEvent.META_SHIFT_ON != 0
    val caps = metaState and KeyEvent.META_CAPS_LOCK_ON != 0
    return when (keyCode) {
      in KeyEvent.KEYCODE_A..KeyEvent.KEYCODE_Z -> {
        val c = 'a' + (keyCode - KeyEvent.KEYCODE_A)
        (if (shift != caps) c.uppercaseChar() else c).code
      }
      in KeyEvent.KEYCODE_0..KeyEvent.KEYCODE_9 -> {
        val d = keyCode - KeyEvent.KEYCODE_0
        (if (shift) SHIFTED_DIGITS[d] else '0' + d).code
      }
      else -> PUNCT[keyCode]?.let { (if (shift) it[1] else it[0]).code } ?: 0
    }
  }
}
