// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.input

import android.view.InputDevice
import android.view.KeyEvent
import android.view.MotionEvent

/** Tunables for [InputTranslator]. */
data class TranslatorConfig(
  /** Send KEYCODE_BACK for Escape instead of KEYCODE_ESCAPE (off: see docs/input.md). */
  val escapeAsBack: Boolean = false,
  /**
   * Display pixels that one AXIS_VSCROLL/HSCROLL unit scrolls (Android's
   * `ViewConfiguration.getScaledVerticalScrollFactor()`, 64 dp). Used to turn smooth (touchpad)
   * scrolling, which the host reports in surface pixels, into scroll units.
   */
  val scrollPixelsPerUnit: Float = 64f,
)

/**
 * Turns the composer's input stream into Android input events, following what Android's own
 * InputReader would produce for a real mouse, touchscreen and keyboard.
 *
 * InputDispatcher gives every injected event the same device id and checks the stream for
 * consistency (InputVerifier), so the mouse and the touchscreen must never overlap: a touch
 * sequence that starts while a mouse button is held is dropped, and mouse events during a touch
 * sequence are dropped (the hover resumes with the next motion).
 *
 * Not thread-safe; feed it from one thread.
 */
class InputTranslator(
  private val config: TranslatorConfig = TranslatorConfig(),
  now: () -> Long = { System.nanoTime() / 1_000_000 },
) {
  private val clock = EventClock(now)
  private val keyboard = Keyboard(config.escapeAsBack)

  var displayWidth = 0
    private set

  var displayHeight = 0
    private set

  private var windowWidth = 0

  // --- pointer state -------------------------------------------------------
  private var x = 0f
  private var y = 0f
  private var hovering = false
  private var buttons = 0 // reported Android button state (PRIMARY/SECONDARY/TERTIARY)
  private var suppressedButtons = 0 // pressed during a touch sequence; never reported
  private var mouseDownTime = 0L

  // Pending changes, applied at pointer_frame.
  private var frameTime = -1L
  private var moved = false
  private var left = false
  private val buttonChanges = ArrayList<Pair<Int, Boolean>>() // evdev code, pressed
  private val v120 = IntArray(2)
  private val hasV120 = BooleanArray(2)
  private val axisValue = DoubleArray(2)

  // --- touch state ---------------------------------------------------------
  private class Touch(val pointerId: Int, var x: Float, var y: Float)

  private val touches = sortedMapOf<Int, Touch>() // by host id; output is sorted by pointer id
  private val suppressedTouches = HashSet<Int>()
  private var touchDownTime = 0L
  private var touchFrameTime = -1L
  private val touchDowns = ArrayList<HostEvent.TouchDown>()
  private val touchMoves = LinkedHashMap<Int, HostEvent.TouchMotion>()
  private val touchUps = ArrayList<Int>()

  /** Translates one event. Returns the Android events to inject, in order. */
  fun handle(e: HostEvent): List<OutEvent> {
    val out = ArrayList<OutEvent>(2)
    when (e) {
      is HostEvent.Hello -> {
        displayWidth = e.displayWidth
        displayHeight = e.displayHeight
      }
      is HostEvent.Window -> windowWidth = e.width
      is HostEvent.PointerEnter -> {
        x = e.x
        y = e.y
        moved = true
        left = false
      }
      HostEvent.PointerLeave -> left = true
      is HostEvent.PointerMotion -> {
        frameTime = clock.map(e.time)
        x = e.x
        y = e.y
        moved = true
      }
      is HostEvent.PointerButton -> {
        frameTime = clock.map(e.time)
        buttonChanges += e.button to e.pressed
      }
      is HostEvent.PointerAxis -> {
        frameTime = clock.map(e.time)
        if (e.axis in 0..1) axisValue[e.axis] += e.value
      }
      is HostEvent.PointerAxisValue120 ->
        if (e.axis in 0..1) {
          v120[e.axis] += e.v120
          hasV120[e.axis] = true
        }
      is HostEvent.PointerAxisStop -> {}
      HostEvent.PointerFrame -> pointerFrame(out)
      HostEvent.KeyboardEnter -> {}
      HostEvent.KeyboardLeave -> keyboard.releaseAll(clock.untimed(), out)
      is HostEvent.Key -> keyboard.key(e.code, e.pressed, clock.map(e.time), out)
      is HostEvent.Modifiers -> keyboard.modifiers(e.depressed, e.latched, e.locked)
      is HostEvent.TouchDown -> {
        touchFrameTime = clock.map(e.time)
        touchDowns += e
      }
      is HostEvent.TouchMotion -> {
        touchFrameTime = clock.map(e.time)
        touchMoves[e.id] = e
      }
      is HostEvent.TouchUp -> {
        touchFrameTime = clock.map(e.time)
        touchUps += e.id
      }
      HostEvent.TouchFrame -> touchFrame(out)
      HostEvent.TouchCancel -> touchCancel(out)
      HostEvent.Close -> {}
    }
    return out
  }

  /** The stream ended (composer gone): release everything that is held. */
  fun reset(): List<OutEvent> {
    val out = ArrayList<OutEvent>()
    clearPointerFrame()
    clearTouchFrame()
    touchCancel(out)
    left = true
    pointerFrame(out)
    keyboard.releaseAll(clock.untimed(), out)
    suppressedButtons = 0
    return out
  }

  // --- pointer -------------------------------------------------------------

  private fun pointerFrame(out: MutableList<OutEvent>) {
    val t = if (frameTime >= 0) frameTime else clock.untimed()
    val meta = keyboard.metaState

    // Side buttons are keys (like InputReader's synthesized BACK/FORWARD); the rest are buttons.
    val sideKeys = ArrayList<Pair<Int, Boolean>>()
    var newButtons = buttons
    for ((code, pressed) in buttonChanges) {
      val key = SIDE_BUTTON_KEYS[code]
      if (key != null) {
        sideKeys += key to pressed
        continue
      }
      val b = BUTTONS[code] ?: continue
      when {
        pressed && touches.isNotEmpty() -> suppressedButtons = suppressedButtons or b
        pressed -> newButtons = newButtons or b
        suppressedButtons and b != 0 -> suppressedButtons = suppressedButtons and b.inv()
        else -> newButtons = newButtons and b.inv()
      }
    }
    if (left) newButtons = 0 // no releases arrive after wl_pointer.leave

    var vscroll = 0f
    var hscroll = 0f
    if (!left) {
      vscroll = -scrollUnits(0)
      hscroll = scrollUnits(1)
    }
    clearPointerFrame()

    for ((key, pressed) in sideKeys) if (pressed) out += sideKey(key, KeyEvent.ACTION_DOWN, t, meta)

    if (touches.isEmpty()) {
      motionFrame(out, t, meta, newButtons, vscroll, hscroll)
    }
    if (left) {
      if (hovering) out += mouse(MotionEvent.ACTION_HOVER_EXIT, 0, 0, t, meta)
      hovering = false
      left = false
    }

    for ((key, pressed) in sideKeys) if (!pressed) out += sideKey(key, KeyEvent.ACTION_UP, t, meta)
  }

  // Mirrors CursorInputMapper::sync: move, releases, DOWN/UP, presses, hover after UP, scroll.
  private fun motionFrame(
    out: MutableList<OutEvent>,
    t: Long,
    meta: Int,
    newButtons: Int,
    vscroll: Float,
    hscroll: Float,
  ) {
    val wasDown = buttons != 0
    val down = newButtons != 0
    val scrolled = vscroll != 0f || hscroll != 0f
    if (!wasDown && down) mouseDownTime = t

    if (moved || (scrolled && !wasDown && !hovering)) {
      if (wasDown) {
        out += mouse(MotionEvent.ACTION_MOVE, 0, buttons, t, meta)
      } else {
        out += mouse(MotionEvent.ACTION_HOVER_MOVE, 0, 0, t, meta)
        hovering = true
      }
    }
    moved = false

    var state = buttons
    for (b in BUTTON_ORDER) {
      if (state and b != 0 && newButtons and b == 0) {
        state = state and b.inv()
        out += mouse(MotionEvent.ACTION_BUTTON_RELEASE, b, state, t, meta)
      }
    }
    if (wasDown && !down) out += mouse(MotionEvent.ACTION_UP, 0, 0, t, meta)
    if (!wasDown && down) {
      out += mouse(MotionEvent.ACTION_DOWN, 0, newButtons, t, meta)
      hovering = false
    }
    for (b in BUTTON_ORDER) {
      if (state and b == 0 && newButtons and b != 0) {
        state = state or b
        out += mouse(MotionEvent.ACTION_BUTTON_PRESS, b, state, t, meta)
      }
    }
    buttons = newButtons
    if (wasDown && !down) {
      out += mouse(MotionEvent.ACTION_HOVER_MOVE, 0, 0, t, meta)
      hovering = true
    }
    if (scrolled) {
      out += mouse(MotionEvent.ACTION_SCROLL, 0, buttons, t, meta, vscroll, hscroll)
    }
  }

  // One detent is one unit. Without value120 (touchpads, kinetic scrolling) convert the
  // surface-pixel distance to display pixels, then to units of scrollPixelsPerUnit.
  private fun scrollUnits(axis: Int): Float {
    if (hasV120[axis]) return v120[axis] / 120f
    if (axisValue[axis] == 0.0) return 0f
    val toDisplay =
      if (windowWidth > 0 && displayWidth > 0) displayWidth.toDouble() / windowWidth else 1.0
    return (axisValue[axis] * toDisplay / config.scrollPixelsPerUnit).toFloat()
  }

  private fun clearPointerFrame() {
    frameTime = -1
    buttonChanges.clear()
    v120.fill(0)
    hasV120.fill(false)
    axisValue.fill(0.0)
  }

  private fun mouse(
    action: Int,
    actionButton: Int,
    buttonState: Int,
    t: Long,
    meta: Int,
    vscroll: Float = 0f,
    hscroll: Float = 0f,
  ): OutMotion {
    // As CursorInputMapper: pressure 1 while any button is down.
    val pressure = if (buttonState != 0) 1f else 0f
    return OutMotion(
      source = InputDevice.SOURCE_MOUSE,
      action = action,
      actionButton = actionButton,
      buttonState = buttonState,
      metaState = meta,
      downTime = if (action == MotionEvent.ACTION_DOWN) t else mouseDownTime,
      eventTime = t,
      pointers =
        listOf(OutPointer(0, MotionEvent.TOOL_TYPE_MOUSE, x, y, pressure, vscroll, hscroll)),
    )
  }

  private fun sideKey(keyCode: Int, action: Int, t: Long, meta: Int) =
    OutKey(action, keyCode, 0, meta, KeyEvent.FLAG_FROM_SYSTEM, t, t)

  // --- touch ---------------------------------------------------------------

  private fun touchFrame(out: MutableList<OutEvent>) {
    val t = if (touchFrameTime >= 0) touchFrameTime else clock.untimed()
    val meta = keyboard.metaState
    val downs = touchDowns.toList()
    val moves = touchMoves.values.toList()
    val ups = touchUps.toList()
    clearTouchFrame()

    // Moves of existing touches: one ACTION_MOVE with every pointer.
    var anyMoved = false
    for (m in moves) {
      val tp = touches[m.id] ?: continue
      if (tp.x != m.x || tp.y != m.y) anyMoved = true
      tp.x = m.x
      tp.y = m.y
    }
    if (anyMoved) out += touch(MotionEvent.ACTION_MOVE, t, meta)

    for (d in downs) {
      if (touches.containsKey(d.id) || d.id in suppressedTouches) continue
      // Drop a touch sequence that starts during a mouse drag, and pointers beyond the limit.
      if ((touches.isEmpty() && buttons != 0) || touches.size >= MAX_POINTERS) {
        suppressedTouches += d.id
        continue
      }
      if (touches.isEmpty()) {
        if (hovering) out += mouse(MotionEvent.ACTION_HOVER_EXIT, 0, 0, t, meta)
        hovering = false
        touchDownTime = t
      }
      val m = moves.lastOrNull { it.id == d.id }
      val tp = Touch(freePointerId(), m?.x ?: d.x, m?.y ?: d.y)
      touches[d.id] = tp
      val action =
        if (touches.size == 1) MotionEvent.ACTION_DOWN
        else MotionEvent.ACTION_POINTER_DOWN or (indexOf(tp) shl INDEX_SHIFT)
      out += touch(action, t, meta)
    }

    for (id in ups) {
      if (suppressedTouches.remove(id)) continue
      val tp = touches[id] ?: continue
      val action =
        if (touches.size == 1) MotionEvent.ACTION_UP
        else MotionEvent.ACTION_POINTER_UP or (indexOf(tp) shl INDEX_SHIFT)
      out += touch(action, t, meta)
      touches.remove(id)
    }
  }

  private fun touchCancel(out: MutableList<OutEvent>) {
    clearTouchFrame()
    suppressedTouches.clear()
    if (touches.isEmpty()) return
    out += touch(MotionEvent.ACTION_CANCEL, clock.untimed(), keyboard.metaState)
    touches.clear()
  }

  private fun clearTouchFrame() {
    touchFrameTime = -1
    touchDowns.clear()
    touchMoves.clear()
    touchUps.clear()
  }

  private fun sortedTouches() = touches.values.sortedBy { it.pointerId }

  private fun indexOf(tp: Touch) = sortedTouches().indexOf(tp)

  private fun freePointerId(): Int {
    val used = touches.values.map { it.pointerId }.toSet()
    return (0 until MAX_POINTERS).first { it !in used }
  }

  private fun touch(action: Int, t: Long, meta: Int) =
    OutMotion(
      source = InputDevice.SOURCE_TOUCHSCREEN,
      action = action,
      actionButton = 0,
      buttonState = 0,
      metaState = meta,
      downTime = touchDownTime,
      eventTime = t,
      pointers =
        sortedTouches().map {
          OutPointer(it.pointerId, MotionEvent.TOOL_TYPE_FINGER, it.x, it.y, 1f)
        },
    )

  private companion object {
    const val INDEX_SHIFT = MotionEvent.ACTION_POINTER_INDEX_SHIFT
    const val MAX_POINTERS = 16 // MAX_POINTERS in Android's Input.h

    // linux/input-event-codes.h
    const val BTN_LEFT = 0x110
    const val BTN_RIGHT = 0x111
    const val BTN_MIDDLE = 0x112
    const val BTN_SIDE = 0x113
    const val BTN_EXTRA = 0x114
    const val BTN_FORWARD = 0x115
    const val BTN_BACK = 0x116

    val BUTTONS =
      mapOf(
        BTN_LEFT to MotionEvent.BUTTON_PRIMARY,
        BTN_RIGHT to MotionEvent.BUTTON_SECONDARY,
        BTN_MIDDLE to MotionEvent.BUTTON_TERTIARY,
      )
    val BUTTON_ORDER =
      intArrayOf(
        MotionEvent.BUTTON_PRIMARY,
        MotionEvent.BUTTON_SECONDARY,
        MotionEvent.BUTTON_TERTIARY,
      )
    val SIDE_BUTTON_KEYS =
      mapOf(
        BTN_SIDE to KeyEvent.KEYCODE_BACK,
        BTN_BACK to KeyEvent.KEYCODE_BACK,
        BTN_EXTRA to KeyEvent.KEYCODE_FORWARD,
        BTN_FORWARD to KeyEvent.KEYCODE_FORWARD,
      )
  }
}
