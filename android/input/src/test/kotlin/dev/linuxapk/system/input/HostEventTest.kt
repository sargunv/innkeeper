// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.input

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull

class HostEventTest {
  @Test
  fun parsesEveryLineInTheProtocol() {
    val cases =
      mapOf(
        "hello 2" to HostEvent.Hello(2),
        "window 1280 800 1.700" to HostEvent.Window(1280, 800, 1.7),
        "pointer_enter 10.50 20.25" to HostEvent.PointerEnter(10.5f, 20.25f),
        "pointer_leave" to HostEvent.PointerLeave(),
        "pointer_motion 4294967295 501.49 564.17" to
          HostEvent.PointerMotion(4294967295L, 501.49f, 564.17f),
        "pointer_button 100 272 1" to HostEvent.PointerButton(100, 272, true),
        "pointer_button 100 272 0" to HostEvent.PointerButton(100, 272, false),
        "pointer_axis 5 0 -15.000" to HostEvent.PointerAxis(5, 0, -15.0),
        "pointer_axis_value120 1 -240" to HostEvent.PointerAxisValue120(1, -240),
        "pointer_axis_stop 7 0" to HostEvent.PointerAxisStop(7, 0),
        "pointer_frame" to HostEvent.PointerFrame,
        "keyboard_enter" to HostEvent.KeyboardEnter(),
        "keyboard_leave" to HostEvent.KeyboardLeave(),
        "key 9 30 1" to HostEvent.Key(9, 30, true),
        "modifiers 1 0 2 0" to HostEvent.Modifiers(1, 0, 2, 0),
        "modifiers 4294967295 0 0 0" to HostEvent.Modifiers(-1, 0, 0, 0),
        "touch_down 1 3 1.00 2.00" to HostEvent.TouchDown(1, 3, 1f, 2f),
        "touch_motion 2 3 4.00 5.00" to HostEvent.TouchMotion(2, 3, 4f, 5f),
        "touch_up 3 3" to HostEvent.TouchUp(3, 3),
        "touch_frame" to HostEvent.TouchFrame,
        "touch_cancel" to HostEvent.TouchCancel,
        "close" to HostEvent.Close(),
      )
    for ((line, expected) in cases) assertEquals(expected, HostEvent.parse(line), line)
  }

  @Test
  fun parsesDisplayIdsAndWindowReplies() {
    val cases =
      mapOf(
        "hello 2 1280 800" to HostEvent.Hello(2),
        "window 1100 700 1.700 5" to HostEvent.Window(1100, 700, 1.7, 5),
        "pointer_enter 10.50 20.25 5" to HostEvent.PointerEnter(10.5f, 20.25f, 5),
        "pointer_leave 5" to HostEvent.PointerLeave(5),
        "keyboard_enter 7" to HostEvent.KeyboardEnter(7),
        "keyboard_leave 7" to HostEvent.KeyboardLeave(7),
        "touch_down 1 3 1.00 2.00 9" to HostEvent.TouchDown(1, 3, 1f, 2f, 9),
        "close 5" to HostEvent.Close(5),
        "window_pending 5" to HostEvent.WindowPending(5),
        "window_mapped 5 100" to HostEvent.WindowMapped(5),
        "window_gone 5" to HostEvent.WindowGone(5),
        "window_failed 5 exists" to HostEvent.WindowFailed(5, "exists"),
      )
    for ((line, expected) in cases) assertEquals(expected, HostEvent.parse(line), line)
  }

  @Test
  fun toleratesWhitespaceAndExtraFields() {
    assertEquals(HostEvent.Key(9, 30, true), HostEvent.parse("  key 9 30 1 extra\r"))
  }

  @Test
  fun rejectsMalformedAndUnknownLines() {
    for (line in
      listOf("", "   ", "bogus 1 2", "key 9 30", "key x 30 1", "pointer_button 1 272 2")) {
      assertNull(HostEvent.parse(line), line)
    }
  }
}
