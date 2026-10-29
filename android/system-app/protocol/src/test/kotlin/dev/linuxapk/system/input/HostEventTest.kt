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
        "key 9 16 1 97 text a" to HostEvent.Key(9, 16, true, 97, "a"),
        "key 9 57 1 32 text %20" to HostEvent.Key(9, 57, true, 32, " "),
        "key 9 3 1 233 text é" to HostEvent.Key(9, 3, true, 233, "é"),
        "key 9 26 1 65106 compose" to HostEvent.Key(9, 26, true, 65106, compose = true),
        "key 9 28 1 65293" to HostEvent.Key(9, 28, true, 65293),
        "key 9 30 2" to HostEvent.Key(9, 30, true, repeat = true),
        "key 9 30 2 97 text a" to HostEvent.Key(9, 30, true, 97, "a", repeat = true),
        "repeat_info 25 600" to HostEvent.RepeatInfo(25, 600),
        "modifiers 1 0 2 0" to HostEvent.Modifiers(1, 0, 2, 0),
        "modifiers 4294967295 0 0 0" to HostEvent.Modifiers(-1, 0, 0, 0),
        "touch_down 1 3 1.00 2.00" to HostEvent.TouchDown(1, 3, 1f, 2f),
        "touch_motion 2 3 4.00 5.00" to HostEvent.TouchMotion(2, 3, 4f, 5f),
        "touch_up 3 3" to HostEvent.TouchUp(3, 3),
        "touch_frame" to HostEvent.TouchFrame,
        "touch_cancel" to HostEvent.TouchCancel,
        "close" to HostEvent.Close(),
        "window 1100 700 1.700 5" to HostEvent.Window(1100, 700, 1.7, 5),
        "pointer_enter 10.50 20.25 5" to HostEvent.PointerEnter(10.5f, 20.25f, 5),
        "pointer_leave 5" to HostEvent.PointerLeave(5),
        "keyboard_enter 7" to HostEvent.KeyboardEnter(7),
        "keyboard_leave 7" to HostEvent.KeyboardLeave(7),
        "touch_down 1 3 1.00 2.00 9" to HostEvent.TouchDown(1, 3, 1f, 2f, 9),
        "close 5" to HostEvent.Close(5),
        "tablet_proximity_in 321 5" to HostEvent.TabletProximityIn(0x141, 5),
        "pointer_axis_source 1" to HostEvent.PointerAxisSource(1),
        "gesture_swipe_begin 5 3" to HostEvent.SwipeBegin(5, 3),
        "gesture_swipe_update 6 1.50 -2.00" to HostEvent.SwipeUpdate(6, 1.5f, -2f),
        "gesture_swipe_end 7 1" to HostEvent.SwipeEnd(7, true),
        "gesture_pinch_begin 5 2" to HostEvent.PinchBegin(5, 2),
        "gesture_pinch_update 6 0.00 1.00 1.2500 -3.50" to
          HostEvent.PinchUpdate(6, 0f, 1f, 1.25, -3.5),
        "gesture_pinch_end 7 0" to HostEvent.PinchEnd(7, false),
        "gesture_hold_begin 5 2" to HostEvent.HoldBegin(5, 2),
        "gesture_hold_end 6 1" to HostEvent.HoldEnd(6, true),
        "tablet_proximity_out" to HostEvent.TabletProximityOut,
        "tablet_down" to HostEvent.TabletDown,
        "tablet_up" to HostEvent.TabletUp,
        "tablet_motion 1.50 2.00" to HostEvent.TabletMotion(1.5f, 2f),
        "tablet_pressure 65535" to HostEvent.TabletPressure(65535),
        "tablet_distance 12" to HostEvent.TabletDistance(12),
        "tablet_tilt -30.00 12.50" to HostEvent.TabletTilt(-30.0, 12.5),
        "tablet_button 331 1" to HostEvent.TabletButton(331, true),
        "tablet_frame 99" to HostEvent.TabletFrame(99),
      )
    for ((line, expected) in cases) assertEquals(expected, HostEvent.parse(line), line)
  }

  @Test
  fun rejectsMalformedAndUnknownLines() {
    for (line in
      listOf(
        "",
        "   ",
        "bogus 1 2",
        "key 9 30",
        "key x 30 1",
        "key 9 30 1 x",
        "key 9 30 3",
        "key 9 30 1 97 text %zz",
        "repeat_info 25",
        "pointer_button 1 272 2",
      )) {
      assertNull(HostEvent.parse(line), line)
    }
  }
}
