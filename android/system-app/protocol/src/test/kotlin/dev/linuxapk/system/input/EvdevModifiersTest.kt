// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.input

import android.view.KeyEvent
import android.view.MotionEvent
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

class EvdevModifiersTest {
  @Test
  fun modifierPressedMidDragUpdatesNativeStateWithoutChangingMouseRoute() {
    val translator = InputTranslator(now = { 1000 })
    val keyboard = EvdevModifiers()
    val records = ArrayList<Triple<Int, Int, Int>>()
    val injected = ArrayList<OutKey>()
    val keys = KeyboardDelivery(injected::add)
    val moves = ArrayList<OutMotion>()
    for (line in
      listOf(
        "keyboard_enter 7",
        "pointer_enter 10 10 7",
        "pointer_frame",
        "pointer_button 1 272 1",
        "pointer_frame",
        "key 2 42 1",
        "pointer_motion 3 20 20",
        "pointer_frame",
        "key 4 42 0",
        "pointer_motion 5 30 30",
        "pointer_frame",
        "pointer_button 6 272 0",
        "pointer_frame",
      )) {
      val events = translator.handle(requireNotNull(HostEvent.parse(line)))
      assertTrue(
        keyboard.sync(translator.metaState) {
          records.addAll(it)
          true
        },
      )
      for (event in events) when (event) {
        is OutKey -> keys.key(event)
        is OutMotion -> {
          assertTrue(EvdevPointer.accepts(event))
          if (event.action == MotionEvent.ACTION_MOVE) moves += event
        }
      }
    }
    assertEquals(listOf(Triple(1, 42, 1), Triple(1, 42, 0)), records)
    assertEquals(
      listOf(KeyEvent.META_SHIFT_ON or KeyEvent.META_SHIFT_LEFT_ON, 0),
      moves.map { it.metaState },
    )
    assertTrue(moves.all { it.buttonState == 1 })
    assertTrue(injected.isEmpty()) // no second modifier KeyEvent from injection
  }

  @Test
  fun lateLeaveFromPreviousDisplayDoesNotReleaseNewWindowsModifier() {
    val translator = InputTranslator(now = { 1000 })
    val keyboard = EvdevModifiers()
    val records = ArrayList<Triple<Int, Int, Int>>()
    // Each HWC display has its own Wayland connection/thread. The old
    // connection can deliver its leave after the new connection's enter/key.
    for (line in
      listOf(
        "keyboard_enter 2",
        "keyboard_enter 4",
        "key 1 42 1",
        "keyboard_leave 2",
        "modifiers 1 0 0 0",
        "key 2 42 0",
        "modifiers 0 0 0 0",
      )) {
      translator.handle(requireNotNull(HostEvent.parse(line)))
      keyboard.sync(translator.metaState) {
        records.addAll(it)
        true
      }
    }
    assertEquals(listOf(Triple(1, 42, 1), Triple(1, 42, 0)), records)
  }

  @Test
  fun inheritedModifiersAndLocksSynchronizeOnlyOnChange() {
    val keyboard = EvdevModifiers()
    val records = ArrayList<Triple<Int, Int, Int>>()
    fun sync(meta: Int) =
      keyboard.sync(meta) {
        records.addAll(it)
        true
      }
    val meta = KeyEvent.META_CTRL_ON or KeyEvent.META_CAPS_LOCK_ON
    sync(meta)
    sync(meta)
    sync(0)
    assertEquals(
      listOf(
        Triple(1, 29, 1),
        Triple(1, 58, 1),
        Triple(1, 58, 0),
        Triple(1, 29, 0),
        Triple(1, 58, 1),
        Triple(1, 58, 0),
      ),
      records,
    )
  }
}
