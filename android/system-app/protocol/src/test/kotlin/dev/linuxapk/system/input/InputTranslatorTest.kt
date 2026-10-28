// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.input

import android.view.InputDevice
import android.view.KeyEvent
import android.view.MotionEvent
import kotlin.random.Random
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

class InputTranslatorTest {
  private var now = 10_000L
  // InputDispatcher verifies each display's stream separately.
  private val verifiers = HashMap<Int, StreamVerifier>()
  private val verifier: StreamVerifier
    get() = verifiers.getOrPut(0) { StreamVerifier() }

  private var tr = InputTranslator(now = { now })

  // Feeds protocol lines; checks every motion event against InputDispatcher's verifier rules.
  private fun feed(vararg lines: String): List<OutEvent> {
    val out = ArrayList<OutEvent>()
    for (l in lines) {
      out += tr.handle(requireNotNull(HostEvent.parse(l)) { l })
    }
    out.filterIsInstance<OutMotion>().forEach {
      verifiers.getOrPut(it.displayId) { StreamVerifier() }.check(it)
    }
    return out
  }

  private fun name(e: OutEvent): String =
    when (e) {
      is OutKey -> (if (e.action == KeyEvent.ACTION_DOWN) "key_down " else "key_up ") + e.keyCode
      is OutMotion -> {
        val a = e.action and MotionEvent.ACTION_MASK
        val idx = e.action shr MotionEvent.ACTION_POINTER_INDEX_SHIFT
        val n =
          when (a) {
            MotionEvent.ACTION_DOWN -> "down"
            MotionEvent.ACTION_UP -> "up"
            MotionEvent.ACTION_MOVE -> "move"
            MotionEvent.ACTION_CANCEL -> "cancel"
            MotionEvent.ACTION_POINTER_DOWN -> "pointer_down($idx)"
            MotionEvent.ACTION_POINTER_UP -> "pointer_up($idx)"
            MotionEvent.ACTION_HOVER_MOVE -> "hover_move"
            MotionEvent.ACTION_SCROLL -> "scroll"
            MotionEvent.ACTION_HOVER_EXIT -> "hover_exit"
            MotionEvent.ACTION_HOVER_ENTER -> "hover_enter"
            MotionEvent.ACTION_BUTTON_PRESS -> "press(${e.actionButton})"
            MotionEvent.ACTION_BUTTON_RELEASE -> "release(${e.actionButton})"
            else -> "action$a"
          }
        "$n state=${e.buttonState}"
      }
    }

  private fun names(events: List<OutEvent>) = events.map(::name)

  @Test
  fun hoverMoveIsAMouseHover() {
    feed("window 1280 800 1.000")
    val e = feed("pointer_motion 1 100.50 200.25", "pointer_frame").single() as OutMotion
    assertEquals(MotionEvent.ACTION_HOVER_MOVE, e.action)
    assertEquals(InputDevice.SOURCE_MOUSE, e.source)
    val p = e.pointers.single()
    assertEquals(MotionEvent.TOOL_TYPE_MOUSE, p.toolType)
    assertEquals(100.5f, p.x)
    assertEquals(200.25f, p.y)
    assertEquals(0f, p.pressure)
  }

  @Test
  fun leftClickFollowsCursorInputMapper() {
    feed("pointer_enter 10.00 10.00", "pointer_frame")
    now += 50
    val press = feed("pointer_button 2 272 1", "pointer_frame")
    assertEquals(
      listOf("down state=1", "press(1) state=1"),
      names(press),
    )
    val down = press[0] as OutMotion
    assertEquals(down.eventTime, down.downTime)
    assertEquals(1f, down.pointers[0].pressure)

    now += 30
    val drag = feed("pointer_motion 3 12.00 11.00", "pointer_frame")
    assertEquals(listOf("move state=1"), names(drag))
    assertEquals(down.downTime, (drag[0] as OutMotion).downTime)

    now += 30
    val release = feed("pointer_button 4 272 0", "pointer_frame")
    assertEquals(listOf("release(1) state=0", "up state=0", "hover_move state=0"), names(release))
    assertEquals(down.downTime, (release[1] as OutMotion).downTime)
    assertEquals(12f, (release[1] as OutMotion).pointers[0].x)
  }

  @Test
  fun secondButtonDuringDragIsAButtonPress() {
    feed("pointer_motion 1 5.00 5.00", "pointer_frame", "pointer_button 2 272 1", "pointer_frame")
    assertEquals(
      listOf("press(2) state=3"),
      names(feed("pointer_button 3 273 1", "pointer_frame")),
    )
    assertEquals(
      listOf("release(1) state=2"),
      names(feed("pointer_button 4 272 0", "pointer_frame")),
    )
    assertEquals(
      listOf("release(2) state=0", "up state=0", "hover_move state=0"),
      names(feed("pointer_button 5 273 0", "pointer_frame")),
    )
  }

  @Test
  fun rightAndMiddleButtons() {
    feed("pointer_motion 1 5.00 5.00", "pointer_frame")
    assertEquals(
      listOf("down state=2", "press(2) state=2"),
      names(feed("pointer_button 2 273 1", "pointer_frame")),
    )
    feed("pointer_button 3 273 0", "pointer_frame")
    assertEquals(
      listOf("down state=4", "press(4) state=4"),
      names(feed("pointer_button 4 274 1", "pointer_frame")),
    )
  }

  @Test
  fun wheelDetentsScrollOneUnitEach() {
    feed("pointer_motion 1 5.00 5.00", "pointer_frame")
    // One detent towards the user (Wayland +) = scroll down = AXIS_VSCROLL -1.
    val e =
      feed("pointer_axis_value120 0 120", "pointer_axis 2 0 15.000", "pointer_frame").single()
        as OutMotion
    assertEquals(MotionEvent.ACTION_SCROLL, e.action)
    assertEquals(-1f, e.pointers[0].vscroll)
    assertEquals(0f, e.pointers[0].hscroll)
    // Horizontal: Wayland + (right) = AXIS_HSCROLL +; hi-res half detent.
    val h = feed("pointer_axis_value120 1 60", "pointer_axis 3 1 7.500", "pointer_frame")
    assertEquals(0.5f, (h.single() as OutMotion).pointers[0].hscroll)
  }

  @Test
  fun scrollBeforeAnyMotionHoversFirst() {
    assertEquals(
      listOf("hover_move state=0", "scroll state=0"),
      names(feed("pointer_axis_value120 0 -120", "pointer_axis 1 0 -15.000", "pointer_frame")),
    )
  }

  @Test
  fun touchpadScrollIsInDisplayPixels() {
    tr = InputTranslator(TranslatorConfig(scrollPixelsPerUnit = 64f), now = { now })
    // A window at scale 2: 16 surface px = 32 display px = half a unit.
    feed("window 640 400 2.000 5", "pointer_enter 1.00 1.00 5", "pointer_frame")
    val e = feed("pointer_axis 2 0 16.000", "pointer_frame").single() as OutMotion
    assertEquals(-0.5f, e.pointers[0].vscroll)
  }

  @Test
  fun sideButtonsAreBackAndForwardKeys() {
    feed("pointer_motion 1 5.00 5.00", "pointer_frame")
    assertEquals(
      listOf("key_down ${KeyEvent.KEYCODE_BACK}"),
      names(feed("pointer_button 2 275 1", "pointer_frame")),
    )
    assertEquals(
      listOf("key_up ${KeyEvent.KEYCODE_BACK}"),
      names(feed("pointer_button 3 275 0", "pointer_frame")),
    )
    assertEquals(
      listOf("key_down ${KeyEvent.KEYCODE_FORWARD}"),
      names(feed("pointer_button 4 276 1", "pointer_frame")),
    )
  }

  @Test
  fun leavingWhilePressedReleasesThenExitsHover() {
    feed("pointer_motion 1 5.00 5.00", "pointer_frame", "pointer_button 2 272 1", "pointer_frame")
    assertEquals(
      listOf("release(1) state=0", "up state=0", "hover_move state=0", "hover_exit state=0"),
      names(feed("pointer_leave", "pointer_frame")),
    )
    // Coming back hovers again; a stale release is ignored.
    assertEquals(
      listOf("hover_move state=0"),
      names(feed("pointer_enter 1.00 1.00", "pointer_frame", "pointer_button 3 272 0")),
    )
  }

  @Test
  fun multiTouch() {
    val d1 = feed("touch_down 1 7 10.00 20.00", "touch_frame")
    assertEquals(listOf("down state=0"), names(d1))
    val m = d1.single() as OutMotion
    assertEquals(InputDevice.SOURCE_TOUCHSCREEN, m.source)
    assertEquals(MotionEvent.TOOL_TYPE_FINGER, m.pointers[0].toolType)
    assertEquals(0, m.pointers[0].id)

    assertEquals(
      listOf("pointer_down(1) state=0"),
      names(feed("touch_down 2 9 30.00 40.00", "touch_frame")),
    )
    val mv = feed("touch_motion 3 7 11.00 21.00", "touch_motion 3 9 31.00 41.00", "touch_frame")
    assertEquals(listOf("move state=0"), names(mv))
    assertEquals(listOf(11f, 31f), (mv.single() as OutMotion).pointers.map { it.x })

    // First finger lifts: pointer 0 at index 0; a new finger then reuses id 0.
    assertEquals(listOf("pointer_up(0) state=0"), names(feed("touch_up 4 7", "touch_frame")))
    val d3 = feed("touch_down 5 3 50.00 60.00", "touch_frame").single() as OutMotion
    assertEquals(MotionEvent.ACTION_POINTER_DOWN, d3.action and MotionEvent.ACTION_MASK)
    assertEquals(listOf(0, 1), d3.pointers.map { it.id })

    feed("touch_up 6 9", "touch_frame")
    val up = feed("touch_up 7 3", "touch_frame").single() as OutMotion
    assertEquals(MotionEvent.ACTION_UP, up.action)
    assertEquals(m.downTime, up.downTime)
    assertTrue(verifier.idle)
  }

  @Test
  fun touchCancel() {
    feed("touch_down 1 1 1.00 1.00", "touch_frame", "touch_down 2 2 2.00 2.00", "touch_frame")
    val c = feed("touch_cancel").single() as OutMotion
    assertEquals(MotionEvent.ACTION_CANCEL, c.action)
    assertEquals(2, c.pointers.size)
    assertTrue(feed("touch_up 3 1", "touch_frame").isEmpty())
  }

  @Test
  fun touchEndsMouseHoverAndMouseWaitsForTouch() {
    feed("pointer_motion 1 5.00 5.00", "pointer_frame")
    assertEquals(
      listOf("hover_exit state=0", "down state=0"),
      names(feed("touch_down 2 1 1.00 1.00", "touch_frame")),
    )
    // Mouse input during the touch sequence is dropped, including a click.
    assertTrue(feed("pointer_motion 3 6.00 6.00", "pointer_frame").isEmpty())
    assertTrue(feed("pointer_button 4 272 1", "pointer_frame").isEmpty())
    feed("touch_up 5 1", "touch_frame")
    assertTrue(
      feed("pointer_button 6 272 0", "pointer_frame").none {
        it is OutMotion && it.action == MotionEvent.ACTION_UP
      },
    )
    assertEquals(
      listOf("hover_move state=0"),
      names(feed("pointer_motion 7 7.00 7.00", "pointer_frame")),
    )
  }

  @Test
  fun touchDuringMouseDragIsDropped() {
    feed("pointer_motion 1 5.00 5.00", "pointer_frame", "pointer_button 2 272 1", "pointer_frame")
    assertTrue(feed("touch_down 3 1 1.00 1.00", "touch_frame").isEmpty())
    assertTrue(
      feed("touch_motion 4 1 2.00 2.00", "touch_frame", "touch_up 5 1", "touch_frame").isEmpty(),
    )
    assertEquals(listOf("move state=1"), names(feed("pointer_motion 6 6.00 6.00", "pointer_frame")))
  }

  @Test
  fun shiftAndLetter() {
    val e =
      feed(
          "key 1 42 1",
          "modifiers 1 0 0 0",
          "key 2 30 1",
          "key 3 30 0",
          "key 4 42 0",
          "modifiers 0 0 0 0",
        )
        .map { it as OutKey }
    val shift = KeyEvent.META_SHIFT_ON or KeyEvent.META_SHIFT_LEFT_ON
    assertEquals(
      listOf(
        Triple(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_SHIFT_LEFT, shift),
        Triple(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_A, shift),
        Triple(KeyEvent.ACTION_UP, KeyEvent.KEYCODE_A, shift),
        Triple(KeyEvent.ACTION_UP, KeyEvent.KEYCODE_SHIFT_LEFT, 0),
      ),
      e.map { Triple(it.action, it.keyCode, it.metaState) },
    )
    assertEquals(30, e[1].scanCode)
    assertEquals(e[1].downTime, e[2].downTime)
    assertTrue(e.all { it.flags and KeyEvent.FLAG_FROM_SYSTEM != 0 })
  }

  @Test
  fun bothShiftsAndRightSideModifiers() {
    val e =
      feed("key 1 42 1", "key 2 54 1", "key 3 42 0", "key 4 100 1", "key 5 97 1").map {
        (it as OutKey).metaState
      }
    val sL = KeyEvent.META_SHIFT_LEFT_ON
    val sR = KeyEvent.META_SHIFT_RIGHT_ON
    val s = KeyEvent.META_SHIFT_ON
    assertEquals(s or sL, e[0])
    assertEquals(s or sL or sR, e[1])
    assertEquals(s or sR, e[2])
    assertEquals(s or sR or KeyEvent.META_ALT_ON or KeyEvent.META_ALT_RIGHT_ON, e[3])
    assertEquals(
      s or
        sR or
        KeyEvent.META_ALT_ON or
        KeyEvent.META_ALT_RIGHT_ON or
        KeyEvent.META_CTRL_ON or
        KeyEvent.META_CTRL_RIGHT_ON,
      e[4],
    )
  }

  @Test
  fun modifiersHeldBeforeFocusComeFromXkb() {
    // Ctrl held on the host before the window got focus: only the modifiers line says so.
    val c = feed("keyboard_enter", "modifiers 4 0 0 0", "key 1 46 1").single() as OutKey
    assertEquals(KeyEvent.META_CTRL_ON or KeyEvent.META_CTRL_LEFT_ON, c.metaState)
    // Super via xkb Mod4, Alt via Mod1.
    val s = feed("modifiers 72 0 0 0", "key 2 47 1").single() as OutKey
    assertEquals(
      KeyEvent.META_META_ON or
        KeyEvent.META_META_LEFT_ON or
        KeyEvent.META_ALT_ON or
        KeyEvent.META_ALT_LEFT_ON,
      s.metaState,
    )
  }

  @Test
  fun staleXkbMaskDoesNotLeakIntoModifierRelease() {
    feed("key 1 29 1", "modifiers 4 0 0 0")
    // Ctrl released: the compositor's modifiers update comes after the key line.
    val up = feed("key 2 29 0").single() as OutKey
    assertEquals(0, up.metaState)
  }

  @Test
  fun locksComeFromXkb() {
    val e = feed("modifiers 0 0 18 0", "key 1 71 1").single() as OutKey
    assertEquals(KeyEvent.META_CAPS_LOCK_ON or KeyEvent.META_NUM_LOCK_ON, e.metaState)
  }

  @Test
  fun mouseEventsCarryKeyboardMeta() {
    feed("key 1 29 1")
    val e = feed("pointer_motion 2 1.00 1.00", "pointer_frame").single() as OutMotion
    assertEquals(KeyEvent.META_CTRL_ON or KeyEvent.META_CTRL_LEFT_ON, e.metaState)
  }

  @Test
  fun focusLossCancelsHeldKeys() {
    feed("key 1 29 1", "key 2 46 1")
    val e = feed("keyboard_leave").map { it as OutKey }
    assertEquals(listOf(KeyEvent.KEYCODE_CTRL_LEFT, KeyEvent.KEYCODE_C), e.map { it.keyCode })
    assertTrue(
      e.all { it.action == KeyEvent.ACTION_UP && it.flags and KeyEvent.FLAG_CANCELED != 0 },
    )
    assertTrue(feed("key 3 46 0").isEmpty())
  }

  @Test
  fun escapeIsEscapeUnlessConfigured() {
    assertEquals(KeyEvent.KEYCODE_ESCAPE, (feed("key 1 1 1").single() as OutKey).keyCode)
    tr = InputTranslator(TranslatorConfig(escapeAsBack = true), now = { now })
    assertEquals(KeyEvent.KEYCODE_BACK, (feed("key 1 1 1").single() as OutKey).keyCode)
  }

  @Test
  fun unmappedKeysAndDuplicatePressesAreDropped() {
    assertTrue(feed("key 1 240 1").isEmpty()) // KEY_UNKNOWN
    feed("key 2 30 1")
    assertTrue(feed("key 3 30 1").isEmpty())
  }

  @Test
  fun resetReleasesEverything() {
    feed("pointer_motion 1 5.00 5.00", "pointer_frame", "pointer_button 2 272 1", "pointer_frame")
    feed("key 3 30 1")
    val out = tr.reset()
    out.filterIsInstance<OutMotion>().forEach(verifier::check)
    assertEquals(
      listOf(
        "release(1) state=0",
        "up state=0",
        "hover_move state=0",
        "hover_exit state=0",
        "key_up ${KeyEvent.KEYCODE_A}",
      ),
      names(out),
    )
    assertTrue(verifier.idle)
  }

  @Test
  fun eventTimesKeepHostSpacingButStayBehindNow() {
    feed("pointer_motion 1000 1.00 1.00", "pointer_frame")
    now += 40
    val a = feed("pointer_motion 1016 2.00 2.00", "pointer_frame").single().eventTime
    val b = feed("pointer_motion 1032 3.00 3.00", "pointer_frame").single().eventTime
    assertEquals(16, b - a)
    assertTrue(b <= now)
    // Host clock far ahead (or wrapped): clamped to now, never backwards.
    val c = feed("pointer_motion 999999 4.00 4.00", "pointer_frame").single().eventTime
    assertEquals(now, c)
    val d = feed("pointer_motion 5 5.00 5.00", "pointer_frame").single().eventTime
    assertTrue(d >= c)
  }

  @Test
  fun clockHandlesU32Wrap() {
    val clock = EventClock({ now })
    val a = clock.map(4294967290L)
    now += 20
    assertEquals(a + 16, clock.map(10L)) // 6 ms before the wrap + 10 after
  }

  // Random host streams never produce a sequence InputDispatcher would reject.
  @Test
  fun randomStreamsStayConsistent() {
    val rnd = Random(1234)
    val buttons = listOf(272, 273, 274, 275, 276)
    val seen = HashSet<Int>()
    repeat(200) {
      tr = InputTranslator(now = { now })
      val v = StreamVerifier()
      val pressed = HashSet<Int>()
      val fingers = HashSet<Int>()
      var t = 0L
      fun line(l: String) {
        val out = tr.handle(HostEvent.parse(l)!!)
        out.filterIsInstance<OutMotion>().forEach { e ->
          seen += e.source * 100 + (e.action and MotionEvent.ACTION_MASK)
          runCatching { v.check(e) }.onFailure { throw AssertionError("after '$l'", it) }
        }
      }
      repeat(300) {
        t += rnd.nextLong(0, 20)
        now += rnd.nextLong(0, 20)
        val x = "%.2f".format(rnd.nextDouble(0.0, 1280.0))
        val y = "%.2f".format(rnd.nextDouble(0.0, 800.0))
        when (rnd.nextInt(17)) {
          0,
          1 -> line("pointer_motion $t $x $y")
          2 -> {
            val b = buttons.random(rnd)
            val p = b !in pressed
            if (p) pressed += b else pressed -= b
            line("pointer_button $t $b ${if (p) 1 else 0}")
          }
          3 -> {
            line("pointer_axis_value120 ${rnd.nextInt(2)} ${rnd.nextInt(-240, 240)}")
            line("pointer_axis $t ${rnd.nextInt(2)} ${rnd.nextDouble(-30.0, 30.0)}")
          }
          4 -> line("pointer_frame")
          5 ->
            if (rnd.nextInt(8) == 0) {
              line("pointer_leave")
              line("pointer_frame")
              pressed.clear()
              line("pointer_enter $x $y")
            }
          6 -> {
            val id = rnd.nextInt(5)
            if (id in fingers) line("touch_up $t $id") else line("touch_down $t $id $x $y")
            if (!fingers.remove(id)) fingers += id
          }
          7 -> fingers.forEach { line("touch_motion $t $it $x $y") }
          8,
          9 -> line("touch_frame")
          10 ->
            if (rnd.nextInt(10) == 0) {
              line("touch_cancel")
              fingers.clear()
            }
          11 -> line("key $t ${rnd.nextInt(1, 128)} ${rnd.nextInt(2)}")
          12 -> {
            line("pointer_axis_source ${rnd.nextInt(3)}")
            if (rnd.nextInt(4) == 0) line("pointer_axis_stop $t ${rnd.nextInt(2)}")
            else line("pointer_axis $t ${rnd.nextInt(2)} ${rnd.nextDouble(-30.0, 30.0)}")
            line("pointer_frame")
          }
          13 -> {
            val g = listOf("swipe", "pinch", "hold").random(rnd)
            when (rnd.nextInt(3)) {
              0 -> line("gesture_${g}_begin $t ${rnd.nextInt(2, 5)}")
              1 -> line("gesture_${g}_end $t ${rnd.nextInt(2)}")
              else ->
                if (g == "pinch") line("gesture_pinch_update $t 1.00 -2.00 ${rnd.nextDouble()} 3.0")
                else line("gesture_swipe_update $t 4.00 1.00")
            }
          }
          14 -> {
            when (rnd.nextInt(6)) {
              0 -> line("tablet_proximity_in ${listOf(0x140, 0x141).random(rnd)}")
              1 -> line("tablet_proximity_out")
              2 -> line(if (rnd.nextBoolean()) "tablet_down" else "tablet_up")
              3 -> line("tablet_motion $x $y")
              4 -> line("tablet_pressure ${rnd.nextInt(65536)}")
              else -> line("tablet_button ${listOf(0x14b, 0x14c).random(rnd)} ${rnd.nextInt(2)}")
            }
          }
          15,
          16 -> line("tablet_frame $t")
        }
      }
      line("pointer_frame")
      line("touch_frame")
      line("tablet_frame $t")
      tr.reset().filterIsInstance<OutMotion>().forEach(v::check)
      assertTrue(v.idle, "not idle after reset")
    }
    // Every mouse, touch and stylus action showed up at least once (including the fake fingers'
    // DOWN, POINTER_DOWN, MOVE, POINTER_UP, UP and CANCEL from the mouse source).
    val stylus = InputDevice.SOURCE_STYLUS or InputDevice.SOURCE_TOUCHSCREEN
    assertTrue(seen.count { it / 100 == InputDevice.SOURCE_MOUSE } >= 11, "only $seen")
    assertTrue(seen.count { it / 100 == InputDevice.SOURCE_TOUCHSCREEN } >= 6, "only $seen")
    assertTrue(seen.count { it / 100 == stylus } >= 8, "only $seen")
  }

  @Test
  fun pointerFollowsTheWindowItEntered() {
    feed("window 800 600 1.500 5", "window 800 600 1.500 7")
    val a = feed("pointer_enter 10.00 10.00 5", "pointer_frame")
    assertEquals(listOf(5), a.map { it.displayId })
    // Drag, then the pointer crosses into window 7 within one frame.
    feed("pointer_button 2 272 1", "pointer_frame")
    val cross = feed("pointer_leave 5", "pointer_enter 3.00 4.00 7", "pointer_frame")
    assertEquals(
      listOf("release(1) state=0", "up state=0", "hover_move state=0", "hover_exit state=0") +
        listOf("hover_move state=0"),
      names(cross),
    )
    assertEquals(listOf(5, 5, 5, 5, 7), cross.map { it.displayId })
    assertTrue(verifiers.getValue(5).idle)
    val click =
      feed("pointer_button 3 272 1", "pointer_frame", "pointer_button 4 272 0", "pointer_frame")
    assertTrue(click.all { it.displayId == 7 })
  }

  @Test
  fun keysGoToTheFocusedWindow() {
    feed("keyboard_enter 5")
    assertEquals(5, feed("key 1 30 1").single().displayId)
    // Focus moves to window 7 with A held: A is cancelled on 5.
    val moved = feed("keyboard_leave 5", "keyboard_enter 7")
    val up = moved.single() as OutKey
    assertEquals(KeyEvent.ACTION_UP, up.action)
    assertEquals(5, up.displayId)
    assertTrue(up.flags and KeyEvent.FLAG_CANCELED != 0)
    assertEquals(7, feed("key 2 48 1").single().displayId)
  }

  @Test
  fun aTouchSequenceStaysOnItsWindow() {
    val down = feed("touch_down 1 0 5.00 5.00 5", "touch_frame")
    assertEquals(listOf(5), down.map { it.displayId })
    // A second finger on another window is dropped, and so is its lift.
    assertTrue(feed("touch_down 2 1 9.00 9.00 7", "touch_frame").isEmpty())
    assertTrue(feed("touch_up 3 1", "touch_frame").isEmpty())
    val up = feed("touch_up 4 0", "touch_frame")
    assertEquals(listOf("up state=0"), names(up))
    assertEquals(5, up.single().displayId)
    // The next sequence may start anywhere.
    assertEquals(7, feed("touch_down 5 1 1.00 1.00 7", "touch_frame").single().displayId)
  }

  @Test
  fun smoothScrollUsesTheWindowScale() {
    tr = InputTranslator(TranslatorConfig(scrollPixelsPerUnit = 64f), now = { now })
    feed("window 800 600 2.000 5", "pointer_enter 1.00 1.00 5", "pointer_frame")
    val e = feed("pointer_axis 1 0 16.000", "pointer_frame").single() as OutMotion
    // 16 surface px = 32 display px = half a unit; Wayland down is VSCROLL -1.
    assertEquals(-0.5f, e.pointers[0].vscroll)
  }

  @Test
  fun hiResWheelStepsAreFractions() {
    feed("pointer_motion 1 5.00 5.00", "pointer_frame")
    val e =
      feed(
          "pointer_axis_source 0",
          "pointer_axis_value120 0 60",
          "pointer_axis 2 0 7.500",
          "pointer_frame",
        )
        .single() as OutMotion
    assertEquals(MotionEvent.ACTION_SCROLL, e.action)
    assertEquals(-0.5f, e.pointers[0].vscroll)
  }

  private fun OutEvent.motion() = this as OutMotion

  private fun axis(e: OutEvent, axis: Int, pointer: Int = 0) =
    e.motion().pointers[pointer].axes[axis] ?: 0f

  // GestureConverter's handleScroll/endScroll: a fake finger that moves against the scroll.
  @Test
  fun fingerScrollIsAFakeFingerSwipe() {
    feed("window 640 400 2.000", "pointer_motion 1 100.00 100.00", "pointer_frame")
    now += 10
    val start = feed("pointer_axis_source 1", "pointer_axis 2 0 5.000", "pointer_frame")
    assertEquals(listOf("hover_exit state=0", "down state=0", "move state=0"), names(start))
    val down = start[1].motion()
    assertEquals(InputDevice.SOURCE_MOUSE, down.source)
    assertEquals(MotionEvent.TOOL_TYPE_FINGER, down.pointers.single().toolType)
    assertEquals(MotionEvent.CLASSIFICATION_TWO_FINGER_SWIPE, down.classification)
    assertEquals(100f, down.pointers[0].y)
    assertEquals(1f, down.pointers[0].pressure)
    // 5 surface px at scale 2 is 10 display px; scrolling down drags the content up.
    val move = start[2].motion()
    assertEquals(90f, move.pointers[0].y)
    assertEquals(10f, axis(move, MotionEvent.AXIS_GESTURE_SCROLL_Y_DISTANCE))
    assertEquals(0f, move.pointers[0].vscroll)

    now += 10
    val next = feed("pointer_axis_source 1", "pointer_axis 3 1 -2.000", "pointer_frame")
    assertEquals(listOf("move state=0"), names(next))
    assertEquals(104f, next[0].motion().pointers[0].x)
    assertEquals(down.downTime, next[0].motion().downTime)

    // The fingers lift: UP (the app flings from the finger's velocity), then the hover.
    now += 10
    val end =
      feed(
        "pointer_axis_source 1",
        "pointer_axis_stop 4 0",
        "pointer_axis_stop 4 1",
        "pointer_frame",
      )
    assertEquals(listOf("up state=0", "hover_move state=0"), names(end))
    assertEquals(MotionEvent.CLASSIFICATION_TWO_FINGER_SWIPE, end[0].motion().classification)
    assertEquals(MotionEvent.CLASSIFICATION_NONE, end[1].motion().classification)
    assertEquals(MotionEvent.TOOL_TYPE_MOUSE, end[1].motion().pointers[0].toolType)
    assertEquals(100f, end[1].motion().pointers[0].y)
  }

  @Test
  fun aFingerScrollContinuesWithoutTheSource() {
    feed("pointer_motion 1 10.00 10.00", "pointer_frame")
    feed("pointer_axis_source 1", "pointer_axis 2 0 5.000", "pointer_frame")
    assertEquals(listOf("move state=0"), names(feed("pointer_axis 3 0 5.000", "pointer_frame")))
    assertEquals(
      listOf("up state=0", "hover_move state=0"),
      names(feed("pointer_axis_stop 4 0", "pointer_frame")),
    )
  }

  @Test
  fun aHoldAfterAScrollStopsTheFling() {
    feed("pointer_motion 1 10.00 10.00", "pointer_frame")
    feed("pointer_axis_source 1", "pointer_axis 2 0 5.000", "pointer_frame")
    feed("pointer_axis_source 1", "pointer_axis_stop 3 0", "pointer_frame")
    val stop = feed("gesture_hold_begin 4 2")
    assertEquals(
      listOf("hover_exit state=0", "down state=0", "cancel state=0", "hover_move state=0"),
      names(stop),
    )
    assertEquals(MotionEvent.CLASSIFICATION_TWO_FINGER_SWIPE, stop[1].motion().classification)
    assertEquals(10f, stop[1].motion().pointers[0].y)
    // Only once per scroll.
    assertTrue(feed("gesture_hold_end 5 1", "gesture_hold_begin 6 2").isEmpty())
  }

  @Test
  fun fingerScrollWhileDraggingScrolls() {
    feed("pointer_motion 1 10.00 10.00", "pointer_frame", "pointer_button 2 272 1", "pointer_frame")
    assertEquals(
      listOf("scroll state=1"),
      names(feed("pointer_axis_source 1", "pointer_axis 3 0 64.000", "pointer_frame")),
    )
  }

  @Test
  fun aClickEndsTheFingerScroll() {
    feed("pointer_motion 1 10.00 10.00", "pointer_frame")
    feed("pointer_axis_source 1", "pointer_axis 2 0 5.000", "pointer_frame")
    assertEquals(
      listOf("up state=0", "hover_move state=0", "down state=1", "press(1) state=1"),
      names(feed("pointer_button 3 272 1", "pointer_frame")),
    )
  }

  @Test
  fun leavingEndsTheFingerScroll() {
    feed("pointer_motion 1 10.00 10.00", "pointer_frame")
    feed("pointer_axis_source 1", "pointer_axis 2 0 5.000", "pointer_frame")
    assertEquals(listOf("up state=0"), names(feed("pointer_leave", "pointer_frame")))
    assertTrue(verifier.idle)
  }

  // GestureConverter's handlePinch/endPinch: two fake fingers around the cursor.
  @Test
  fun pinchIsTwoFakeFingers() {
    feed("pointer_motion 1 300.00 200.00", "pointer_frame")
    val begin = feed("gesture_pinch_begin 2 2")
    assertEquals(
      listOf("hover_exit state=0", "down state=0", "pointer_down(1) state=0"),
      names(begin),
    )
    val pd = begin[2].motion()
    assertEquals(MotionEvent.CLASSIFICATION_PINCH, pd.classification)
    assertEquals(listOf(200f, 400f), pd.pointers.map { it.x })
    assertEquals(1f, axis(pd, MotionEvent.AXIS_GESTURE_PINCH_SCALE_FACTOR))

    val zoom = feed("gesture_pinch_update 3 0.00 0.00 1.5000 0.00").single().motion()
    assertEquals(MotionEvent.ACTION_MOVE, zoom.action)
    assertEquals(1.5f, axis(zoom, MotionEvent.AXIS_GESTURE_PINCH_SCALE_FACTOR))
    assertEquals(listOf(150f, 450f), zoom.pointers.map { it.x })
    // The factor is since the last event; Wayland's scale is since the begin.
    val back = feed("gesture_pinch_update 4 0.00 0.00 0.7500 0.00").single().motion()
    assertEquals(0.5f, axis(back, MotionEvent.AXIS_GESTURE_PINCH_SCALE_FACTOR))

    val end = feed("gesture_pinch_end 5 0")
    assertEquals(
      listOf("pointer_up(1) state=0", "up state=0", "hover_move state=0"),
      names(end),
    )
    assertEquals(MotionEvent.CLASSIFICATION_PINCH, end[1].motion().classification)
    assertEquals(begin[1].motion().downTime, end[1].motion().downTime)
  }

  @Test
  fun pinchRotationTurnsTheFingers() {
    feed("pointer_motion 1 300.00 200.00", "pointer_frame", "gesture_pinch_begin 2 2")
    val turned = feed("gesture_pinch_update 3 0.00 0.00 1.0000 90.00").single().motion()
    assertEquals(300f, turned.pointers[0].x, 0.01f)
    assertEquals(100f, turned.pointers[0].y, 0.01f)
    assertEquals(300f, turned.pointers[1].x, 0.01f)
    assertEquals(300f, turned.pointers[1].y, 0.01f)
  }

  @Test
  fun aCancelledPinchIsCancelled() {
    feed("pointer_motion 1 30.00 20.00", "pointer_frame", "gesture_pinch_begin 2 2")
    assertEquals(
      listOf("cancel state=0", "hover_move state=0"),
      names(feed("gesture_pinch_end 3 1")),
    )
  }

  // GestureConverter's handleMultiFingerSwipe/Lift.
  @Test
  fun threeFingerSwipe() {
    feed("window 1000 500 1.000", "pointer_motion 1 300.00 200.00", "pointer_frame")
    val begin = feed("gesture_swipe_begin 2 3")
    assertEquals(
      listOf(
        "hover_exit state=0",
        "down state=0",
        "pointer_down(1) state=0",
        "pointer_down(2) state=0",
      ),
      names(begin),
    )
    assertEquals(listOf(200f, 300f, 400f), begin[3].motion().pointers.map { it.x })
    assertEquals(
      InputTranslator.CLASSIFICATION_MULTI_FINGER_SWIPE,
      begin[3].motion().classification,
    )
    assertEquals(3f, axis(begin[1], InputTranslator.AXIS_GESTURE_SWIPE_FINGER_COUNT))
    val move = feed("gesture_swipe_update 3 50.00 0.00").single()
    assertEquals(listOf(250f, 350f, 450f), move.motion().pointers.map { it.x })
    assertEquals(0.05f, axis(move, MotionEvent.AXIS_GESTURE_X_OFFSET))
    assertEquals(
      listOf("pointer_up(2) state=0", "pointer_up(1) state=0", "up state=0", "hover_move state=0"),
      names(feed("gesture_swipe_end 4 0")),
    )
  }

  @Test
  fun touchDuringAGestureIsDroppedAndGesturesWaitForTouch() {
    feed("pointer_motion 1 30.00 20.00", "pointer_frame", "gesture_pinch_begin 2 2")
    assertTrue(feed("touch_down 3 1 1.00 1.00", "touch_frame").isEmpty())
    feed("gesture_pinch_end 4 0", "touch_up 5 1", "touch_frame")
    feed("touch_down 6 2 1.00 1.00", "touch_frame")
    assertTrue(feed("gesture_pinch_begin 7 2", "gesture_pinch_update 8 0 0 2.0 0").isEmpty())
    assertTrue(
      feed("pointer_axis_source 1", "pointer_axis 9 0 5.000", "pointer_frame").isEmpty(),
    )
  }

  // TouchInputMapper's stylus stream.
  @Test
  fun penHoverDownPressureUp() {
    feed("pointer_motion 1 5.00 5.00", "pointer_frame")
    val enter =
      feed(
        "tablet_proximity_in 320",
        "tablet_motion 100.00 50.00",
        "tablet_distance 6553",
        "tablet_tilt 30.00 0.00",
        "tablet_frame 2",
      )
    assertEquals(
      listOf("hover_exit state=0", "hover_enter state=0", "hover_move state=0"),
      names(enter),
    )
    val hover = enter[2].motion()
    assertEquals(InputDevice.SOURCE_STYLUS or InputDevice.SOURCE_TOUCHSCREEN, hover.source)
    assertEquals(InputDevice.SOURCE_MOUSE, enter[0].motion().source)
    val p = hover.pointers.single()
    assertEquals(MotionEvent.TOOL_TYPE_STYLUS, p.toolType)
    assertEquals(100f, p.x)
    assertEquals(0f, p.pressure)
    assertEquals(0.1f, axis(hover, MotionEvent.AXIS_DISTANCE), 0.001f)
    assertEquals(Math.toRadians(30.0).toFloat(), axis(hover, MotionEvent.AXIS_TILT), 0.001f)

    // The mouse waits while the pen is in proximity.
    assertTrue(feed("pointer_motion 3 6.00 6.00", "pointer_frame").isEmpty())

    val down = feed("tablet_down", "tablet_pressure 32768", "tablet_frame 4")
    assertEquals(listOf("hover_exit state=0", "down state=0"), names(down))
    assertEquals(0.5f, down[1].motion().pointers[0].pressure, 0.001f)
    val stroke = feed("tablet_motion 110.00 55.00", "tablet_pressure 65535", "tablet_frame 5")
    assertEquals(listOf("move state=0"), names(stroke))
    assertEquals(1f, stroke[0].motion().pointers[0].pressure)
    assertEquals(down[1].motion().downTime, stroke[0].motion().downTime)
    val up = feed("tablet_up", "tablet_pressure 0", "tablet_frame 6")
    assertEquals(
      listOf("move state=0", "up state=0", "hover_enter state=0", "hover_move state=0"),
      names(up),
    )
    assertEquals(
      listOf("hover_exit state=0"),
      names(feed("tablet_proximity_out", "tablet_frame 7")),
    )
    assertTrue(verifier.idle)
    assertEquals(
      listOf("hover_move state=0"),
      names(feed("pointer_motion 8 6.00 6.00", "pointer_frame")),
    )
  }

  @Test
  fun eraserAndStylusButtons() {
    val enter = feed("tablet_proximity_in 321 4", "tablet_motion 1.00 1.00", "tablet_frame 1")
    assertEquals(MotionEvent.TOOL_TYPE_ERASER, enter.last().motion().pointers[0].toolType)
    assertEquals(4, enter.last().displayId)
    val press = feed("tablet_button 331 1", "tablet_frame 2")
    assertEquals(listOf("press(${MotionEvent.BUTTON_STYLUS_PRIMARY}) state=32"), names(press))
    assertEquals(
      listOf("hover_exit state=32", "down state=32"),
      names(feed("tablet_down", "tablet_pressure 1000", "tablet_frame 3")),
    )
    // Leaving with the tip down and the button held lifts and releases.
    assertEquals(
      listOf("release(32) state=0", "up state=0"),
      names(feed("tablet_proximity_out", "tablet_frame 4")),
    )
    assertTrue(verifiers.getValue(4).idle)
  }

  @Test
  fun penAndTouchAndDragNeverOverlap() {
    feed("tablet_proximity_in 320", "tablet_motion 1.00 1.00", "tablet_frame 1")
    assertTrue(feed("touch_down 2 1 5.00 5.00", "touch_frame").isEmpty())
    feed("touch_up 3 1", "touch_frame", "tablet_proximity_out", "tablet_frame 4")
    // A pen that comes during a drag is ignored until it leaves.
    feed("pointer_motion 5 5.00 5.00", "pointer_frame", "pointer_button 6 272 1", "pointer_frame")
    assertTrue(
      feed("tablet_proximity_in 320", "tablet_motion 1.00 1.00", "tablet_frame 7").isEmpty(),
    )
    assertEquals(
      listOf("release(1) state=0", "up state=0", "hover_move state=0"),
      names(feed("pointer_button 8 272 0", "pointer_frame")),
    )
    assertTrue(feed("tablet_down", "tablet_frame 9", "tablet_up", "tablet_frame 10").isEmpty())
    feed("tablet_proximity_out", "tablet_frame 11")
    assertEquals(
      listOf("hover_exit state=0", "hover_enter state=0", "hover_move state=0"),
      names(feed("tablet_proximity_in 320", "tablet_motion 1.00 1.00", "tablet_frame 12")),
    )
  }

  @Test
  fun resetEndsGesturesAndThePen() {
    feed("pointer_motion 1 30.00 20.00", "pointer_frame", "gesture_pinch_begin 2 2")
    tr.reset().filterIsInstance<OutMotion>().forEach(verifier::check)
    assertTrue(verifier.idle)
    feed("tablet_proximity_in 320", "tablet_down", "tablet_motion 1.00 1.00", "tablet_frame 3")
    tr.reset().filterIsInstance<OutMotion>().forEach(verifier::check)
    assertTrue(verifier.idle)
  }
}
