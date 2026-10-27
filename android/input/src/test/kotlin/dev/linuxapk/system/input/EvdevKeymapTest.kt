// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.input

import android.view.KeyEvent
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

class EvdevKeymapTest {
  @Test
  fun mapsCommonDesktopKeys() {
    // evdev code (linux/input-event-codes.h) to the KeyEvent constant.
    val cases =
      mapOf(
        1 to KeyEvent.KEYCODE_ESCAPE,
        2 to KeyEvent.KEYCODE_1,
        11 to KeyEvent.KEYCODE_0,
        12 to KeyEvent.KEYCODE_MINUS,
        13 to KeyEvent.KEYCODE_EQUALS,
        14 to KeyEvent.KEYCODE_DEL, // Backspace
        15 to KeyEvent.KEYCODE_TAB,
        16 to KeyEvent.KEYCODE_Q,
        28 to KeyEvent.KEYCODE_ENTER,
        29 to KeyEvent.KEYCODE_CTRL_LEFT,
        30 to KeyEvent.KEYCODE_A,
        41 to KeyEvent.KEYCODE_GRAVE,
        42 to KeyEvent.KEYCODE_SHIFT_LEFT,
        43 to KeyEvent.KEYCODE_BACKSLASH,
        44 to KeyEvent.KEYCODE_Z,
        54 to KeyEvent.KEYCODE_SHIFT_RIGHT,
        55 to KeyEvent.KEYCODE_NUMPAD_MULTIPLY,
        56 to KeyEvent.KEYCODE_ALT_LEFT,
        57 to KeyEvent.KEYCODE_SPACE,
        58 to KeyEvent.KEYCODE_CAPS_LOCK,
        59 to KeyEvent.KEYCODE_F1,
        68 to KeyEvent.KEYCODE_F10,
        69 to KeyEvent.KEYCODE_NUM_LOCK,
        70 to KeyEvent.KEYCODE_SCROLL_LOCK,
        71 to KeyEvent.KEYCODE_NUMPAD_7,
        87 to KeyEvent.KEYCODE_F11,
        88 to KeyEvent.KEYCODE_F12,
        96 to KeyEvent.KEYCODE_NUMPAD_ENTER,
        97 to KeyEvent.KEYCODE_CTRL_RIGHT,
        99 to KeyEvent.KEYCODE_SYSRQ, // Print Screen
        100 to KeyEvent.KEYCODE_ALT_RIGHT,
        102 to KeyEvent.KEYCODE_MOVE_HOME,
        103 to KeyEvent.KEYCODE_DPAD_UP,
        104 to KeyEvent.KEYCODE_PAGE_UP,
        105 to KeyEvent.KEYCODE_DPAD_LEFT,
        106 to KeyEvent.KEYCODE_DPAD_RIGHT,
        107 to KeyEvent.KEYCODE_MOVE_END,
        108 to KeyEvent.KEYCODE_DPAD_DOWN,
        109 to KeyEvent.KEYCODE_PAGE_DOWN,
        110 to KeyEvent.KEYCODE_INSERT,
        111 to KeyEvent.KEYCODE_FORWARD_DEL, // Delete
        113 to KeyEvent.KEYCODE_VOLUME_MUTE,
        114 to KeyEvent.KEYCODE_VOLUME_DOWN,
        115 to KeyEvent.KEYCODE_VOLUME_UP,
        119 to KeyEvent.KEYCODE_BREAK, // Pause
        125 to KeyEvent.KEYCODE_META_LEFT,
        126 to KeyEvent.KEYCODE_META_RIGHT,
        127 to KeyEvent.KEYCODE_MENU, // Compose
        158 to KeyEvent.KEYCODE_BACK,
        164 to KeyEvent.KEYCODE_MEDIA_PLAY_PAUSE,
        163 to KeyEvent.KEYCODE_MEDIA_NEXT,
        165 to KeyEvent.KEYCODE_MEDIA_PREVIOUS,
      )
    for ((evdev, keycode) in cases) {
      assertEquals(keycode, EvdevKeymap.keycode(evdev), "evdev $evdev")
    }
  }

  @Test
  fun everyLetterAndDigitIsMapped() {
    // QWERTY rows as evdev codes.
    val rows = listOf(16..25 to "QWERTYUIOP", 30..38 to "ASDFGHJKL", 44..50 to "ZXCVBNM")
    for ((codes, letters) in rows) {
      for ((code, letter) in codes.zip(letters.toList())) {
        assertEquals(KeyEvent.KEYCODE_A + (letter - 'A'), EvdevKeymap.keycode(code), "$letter")
      }
    }
    for (d in 1..9) assertEquals(KeyEvent.KEYCODE_0 + d, EvdevKeymap.keycode(1 + d))
  }

  @Test
  fun unknownCodesMapToUnknown() {
    assertEquals(KeyEvent.KEYCODE_UNKNOWN, EvdevKeymap.keycode(0))
    assertEquals(KeyEvent.KEYCODE_UNKNOWN, EvdevKeymap.keycode(0x110)) // BTN_LEFT
    assertEquals(KeyEvent.KEYCODE_UNKNOWN, EvdevKeymap.keycode(100_000))
    assertTrue(EvdevKeymap.SIZE > 250)
  }
}
