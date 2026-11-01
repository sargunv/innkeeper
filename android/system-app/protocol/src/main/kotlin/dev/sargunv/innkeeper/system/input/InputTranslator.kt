// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.input

import android.view.InputDevice
import android.view.KeyEvent
import android.view.MotionEvent

/** Tunables for [InputTranslator]. */
data class TranslatorConfig(
  /**
   * Send KEYCODE_BACK for Escape instead of KEYCODE_ESCAPE. It is off by default (docs/input.md).
   */
  val escapeAsBack: Boolean = false,
  /**
   * Display pixels that one AXIS_VSCROLL/HSCROLL unit scrolls (Android's
   * `ViewConfiguration.getScaledVerticalScrollFactor()`, 64 dp). Used to turn smooth (touchpad)
   * scrolling, which the host reports in surface pixels, into scroll units.
   */
  val scrollPixelsPerUnit: Float = 64f,
  /** What the injected keys' key character map types ([Keyboard]). */
  val keyChars: KeyChars = KeyChars { _, _ -> 0 },
  /** Ctrl+W and Ctrl+Q close the window, and Alt+Left is BACK ([Keyboard]). */
  val windowShortcuts: Boolean = false,
)

/**
 * Turns the composer's input stream into Android input events, following what Android's own
 * InputReader would produce for a real mouse, touchscreen and keyboard.
 *
 * InputDispatcher gives every injected event the same device id and checks the stream for
 * consistency (InputVerifier), so the mouse, the touchscreen and the pen must never overlap. A
 * touch sequence or pen that starts while a mouse button is held or a touchpad gesture runs is
 * dropped. Mouse events during a touch sequence or while the pen is in proximity are dropped, and
 * the hover resumes with the next motion.
 *
 * Touchpad scrolling (finger axis source) and gestures follow Android's touchpad mapping in
 * InputReader's GestureConverter. It produces fake fingers from a mouse source with a
 * classification, and apps scroll, fling and zoom with them as they do for touch. The pen follows
 * TouchInputMapper's stylus events.
 *
 * Each window is one Android display. Events go to the display whose window the pointer, the
 * keyboard focus or the first finger of a touch sequence entered. Moving the pointer to another
 * window first ends the hover and any drag on the old display. A touch sequence stays on one
 * display, so fingers that land on another window during it are dropped.
 *
 * It is not thread-safe, so feed it from one thread.
 */
class InputTranslator(
  private val config: TranslatorConfig = TranslatorConfig(),
  now: () -> Long = { System.nanoTime() / 1_000_000 },
  /** Whether the app on this display keeps Ctrl+W (a web browser closes a tab with it). */
  private val keepsCtrlW: (display: Int) -> Boolean = { false },
  private val escapeBack: (display: Int) -> Boolean = { config.escapeAsBack },
  /** A window shortcut asked to close the window of this display. */
  private val onClose: (display: Int) -> Unit = {},
) {
  private val clock = EventClock(now)
  private val keyboard =
    Keyboard(
      config.escapeAsBack,
      config.keyChars,
      config.windowShortcuts,
      keepCtrlW = { keepsCtrlW(keyDisplay) },
      onClose = { onClose(keyDisplay) },
      escapeBack = { escapeBack(keyDisplay) },
    )

  private val windows = HashMap<Int, HostEvent.Window>() // by display

  /** Display that pointer, keyboard and touch events currently go to. */
  var pointerDisplay = 0
    private set

  var keyDisplay = 0
    private set

  /** Effective host modifiers, independent of whether a key produced an output event. */
  val metaState: Int
    get() = keyboard.metaState

  private var touchDisplay = 0

  // --- pointer state -------------------------------------------------------
  private var x = 0f
  private var y = 0f
  private var hovering = false
  private var buttons = 0 // reported Android button state (PRIMARY/SECONDARY/TERTIARY)
  private var suppressedButtons = 0 // pressed during a touch sequence, so never reported
  private var mouseDownTime = 0L

  // Pending changes, applied at pointer_frame.
  private var frameTime = -1L
  private var moved = false
  private var left = false
  private val buttonChanges = ArrayList<Pair<Int, Boolean>>() // evdev code, pressed
  private val v120 = IntArray(2)
  private val hasV120 = BooleanArray(2)
  private val axisValue = DoubleArray(2)
  private var axisSource = -1
  private var axisStop = false

  // --- touchpad gestures: GestureConverter's fake fingers ------------------
  private enum class Gesture {
    NONE,
    SCROLL,
    PINCH,
    SWIPE,
  }

  private var gesture = Gesture.NONE
  private var gestureDownTime = 0L
  private var gestureFingers = 0
  private val fingerX = FloatArray(MAX_SWIPE_FINGERS)
  private val fingerY = FloatArray(MAX_SWIPE_FINGERS)
  private var pinchScale = 1.0 // relative to the begin
  private var pinchAngle = 0.0 // radians, clockwise
  private var pinchX = 0f
  private var pinchY = 0f
  private var flingMayBeInProgress = false

  // --- pen (tablet tool) ---------------------------------------------------
  private var penIn = false // in proximity, injected
  private var penIgnored = false // in proximity but dropped, since it came during a drag or touch
  private var penTool = MotionEvent.TOOL_TYPE_STYLUS
  private var penDisplay = 0
  private var penX = 0f
  private var penY = 0f
  private var penPressure = 0f
  private var penDistance = -1f // -1 if not reported
  private var penTiltX = Double.NaN // degrees, or NaN if not reported
  private var penTiltY = 0.0
  private var penDown = false
  private var penLast: OutPointer? = null // the last sample in contact, which the lift repeats
  private var penHovering = false
  private var penButtons = 0
  private var penDownTime = 0L

  // Pending changes, applied at tablet_frame.
  private var penFrameTime = -1L
  private var penEnter: HostEvent.TabletProximityIn? = null
  private var penLeave = false
  private var penTip: Boolean? = null
  private var penChanged = false
  private val penButtonChanges = ArrayList<Pair<Int, Boolean>>()

  // --- touch state ---------------------------------------------------------
  private class Touch(val pointerId: Int, var x: Float, var y: Float)

  private val touches = sortedMapOf<Int, Touch>() // by host id. Output is sorted by pointer id.
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
      is HostEvent.Window -> windows[e.display] = e
      is HostEvent.PointerEnter -> {
        if (
          e.display != pointerDisplay &&
            (left || hovering || buttons != 0 || gesture != Gesture.NONE)
        ) {
          // Finish on the old window first with button releases and HOVER_EXIT.
          left = true
          pointerFrame(out)
        }
        pointerDisplay = e.display
        x = e.x
        y = e.y
        moved = true
        left = false
      }
      is HostEvent.PointerLeave -> left = true
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
      is HostEvent.PointerAxisStop -> {
        frameTime = clock.map(e.time)
        axisStop = true
      }
      is HostEvent.PointerAxisSource -> axisSource = e.source
      HostEvent.PointerFrame -> pointerFrame(out)
      is HostEvent.SwipeBegin -> swipeBegin(out, clock.map(e.time), e.fingers)
      is HostEvent.SwipeUpdate -> swipeUpdate(out, clock.map(e.time), e.dx, e.dy)
      is HostEvent.SwipeEnd -> gestureEnd(out, clock.map(e.time), Gesture.SWIPE, e.cancelled)
      is HostEvent.PinchBegin -> pinchBegin(out, clock.map(e.time))
      is HostEvent.PinchUpdate -> pinchUpdate(out, clock.map(e.time), e)
      is HostEvent.PinchEnd -> gestureEnd(out, clock.map(e.time), Gesture.PINCH, e.cancelled)
      is HostEvent.HoldBegin -> holdBegin(out, clock.map(e.time))
      is HostEvent.HoldEnd -> {}
      is HostEvent.TabletProximityIn -> {
        penEnter = e
        penLeave = false
      }
      HostEvent.TabletProximityOut -> penLeave = true
      HostEvent.TabletDown -> penTip = true
      HostEvent.TabletUp -> penTip = false
      is HostEvent.TabletMotion -> {
        penX = e.x
        penY = e.y
        penChanged = true
      }
      is HostEvent.TabletPressure -> {
        penPressure = e.pressure.coerceIn(0, 65535) / 65535f
        penChanged = true
      }
      is HostEvent.TabletDistance -> {
        penDistance = e.distance.coerceIn(0, 65535) / 65535f
        penChanged = true
      }
      is HostEvent.TabletTilt -> {
        penTiltX = e.x
        penTiltY = e.y
        penChanged = true
      }
      is HostEvent.TabletButton -> penButtonChanges += e.button to e.pressed
      is HostEvent.TabletFrame -> {
        penFrameTime = clock.map(e.time)
        penFrame(out)
      }
      is HostEvent.KeyboardEnter -> {
        if (e.display != keyDisplay) keys(out) { keyboard.releaseAll(clock.untimed(), it) }
        keyDisplay = e.display
      }
      is HostEvent.KeyboardLeave -> {
        // Display connections run on separate Wayland threads. An old
        // display's leave can arrive after the new focus's enter and keys.
        if (e.display == -1 || e.display == keyDisplay)
          keys(out) { keyboard.releaseAll(clock.untimed(), it) }
      }
      // A compositor's repeats may carry the press's time, but they happen now.
      is HostEvent.Key ->
        keys(out) { keyboard.key(e, if (e.repeat) clock.untimed() else clock.map(e.time), it) }
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
      is HostEvent.Back -> {
        val t = clock.untimed()
        out += sideKey(KeyEvent.KEYCODE_BACK, KeyEvent.ACTION_DOWN, t, 0, e.display)
        out += sideKey(KeyEvent.KEYCODE_BACK, KeyEvent.ACTION_UP, t, 0, e.display)
      }
      is HostEvent.Close,
      is HostEvent.RepeatInfo,
      is HostEvent.WindowOutputs,
      is HostEvent.WindowVisible,
      is HostEvent.WindowMapped,
      is HostEvent.PointerRelative, // VirtualInput sends these straight to evdev.
      is HostEvent.WindowUnmapped,
      is HostEvent.TextDelete,
      is HostEvent.TextCommit,
      is HostEvent.TextPreedit,
      is HostEvent.TextDone -> {}
    }
    return out
  }

  // Keyboard events go to the display with the keyboard focus.
  private inline fun keys(out: MutableList<OutEvent>, f: (MutableList<OutEvent>) -> Unit) {
    val tmp = ArrayList<OutEvent>(2)
    f(tmp)
    for (e in tmp) out += if (e is OutKey) e.copy(displayId = keyDisplay) else e
  }

  /** The stream ended because the composer is gone. Releases everything that is held. */
  fun reset(): List<OutEvent> {
    val out = ArrayList<OutEvent>()
    clearPointerFrame()
    clearTouchFrame()
    clearPenFrame()
    penLeave = true
    penFrame(out)
    touchCancel(out)
    left = true
    pointerFrame(out)
    keys(out) { keyboard.releaseAll(clock.untimed(), it) }
    suppressedButtons = 0
    return out
  }

  // --- pointer -------------------------------------------------------------

  // Mouse input waits while a touch sequence or the pen owns the stream.
  private fun mouseBlocked() = touches.isNotEmpty() || penIn

  private fun pointerFrame(out: MutableList<OutEvent>) {
    // Use the newest time handed out, since another device's frame may have come in between.
    val t = if (frameTime >= 0) clock.current() else clock.untimed()
    val meta = keyboard.metaState

    // Side buttons are keys, like InputReader's synthesized BACK/FORWARD. The rest are buttons.
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
        pressed && mouseBlocked() -> suppressedButtons = suppressedButtons or b
        pressed -> newButtons = newButtons or b
        suppressedButtons and b != 0 -> suppressedButtons = suppressedButtons and b.inv()
        else -> newButtons = newButtons and b.inv()
      }
    }
    if (left) newButtons = 0 // no releases arrive after wl_pointer.leave

    // Finger scrolling moves a fake finger unless a button is held, in which case it scrolls.
    // Other scrolling is ACTION_SCROLL. Not every frame of a finger scroll names its source.
    val finger =
      (axisSource == AXIS_SOURCE_FINGER || (axisSource < 0 && gesture == Gesture.SCROLL)) &&
        buttons == 0 &&
        newButtons == 0
    var vscroll = 0f
    var hscroll = 0f
    var dx = 0f
    var dy = 0f
    if (!left) {
      if (finger) {
        dx = displayPixels(1)
        dy = displayPixels(0)
      } else {
        vscroll = -scrollUnits(0)
        hscroll = scrollUnits(1)
      }
    }
    val stop = axisStop
    clearPointerFrame()

    // A touchpad gesture ends before anything else the pointer does.
    if (
      gesture != Gesture.NONE &&
        (left ||
          moved ||
          newButtons != buttons ||
          sideKeys.isNotEmpty() ||
          vscroll != 0f ||
          hscroll != 0f ||
          (gesture != Gesture.SCROLL && (dx != 0f || dy != 0f)))
    ) {
      endGesture(out, t)
    }

    for ((key, pressed) in sideKeys) if (pressed) out += sideKey(key, KeyEvent.ACTION_DOWN, t, meta)

    if (!mouseBlocked()) {
      motionFrame(out, t, meta, newButtons, vscroll, hscroll)
      if (dx != 0f || dy != 0f) scroll(out, t, dx, dy)
    }
    if (stop && gesture == Gesture.SCROLL) endGesture(out, t)
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

  // One detent is one unit (a hi-res wheel's value120 gives fractions, as CursorInputMapper's
  // hi-res scroll). Without value120 (continuous sources) convert the surface-pixel distance to
  // display pixels, then to units of scrollPixelsPerUnit.
  private fun scrollUnits(axis: Int): Float {
    if (hasV120[axis]) return v120[axis] / 120f
    return displayPixels(axis) / config.scrollPixelsPerUnit
  }

  // The window manager sizes each display to its window's logical size times the window's scale.
  private fun displayPixels(axis: Int): Float {
    if (axisValue[axis] == 0.0) return 0f
    return (axisValue[axis] * (windows[pointerDisplay]?.scale ?: 1.0)).toFloat()
  }

  private fun clearPointerFrame() {
    frameTime = -1
    buttonChanges.clear()
    v120.fill(0)
    hasV120.fill(false)
    axisValue.fill(0.0)
    axisSource = -1
    axisStop = false
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
    // Like CursorInputMapper, report pressure 1 while any button is down.
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
      displayId = pointerDisplay,
    )
  }

  private fun sideKey(
    keyCode: Int,
    action: Int,
    t: Long,
    meta: Int,
    display: Int = pointerDisplay,
  ) = OutKey(action, keyCode, 0, meta, KeyEvent.FLAG_FROM_SYSTEM, t, t, display)

  // --- touchpad gestures ---------------------------------------------------
  // This follows GestureConverter's state machine (handleScroll/endScroll, handleFling,
  // handlePinch/endPinch, handleMultiFingerSwipe/Lift). The hover exits, fake fingers go down
  // around the cursor, move and lift, and the hover resumes. The app makes its own fling from the
  // fake finger's motion.

  private fun gestureAllowed() = !left && buttons == 0 && touches.isEmpty() && !penIn && !penIgnored

  private fun exitHover(out: MutableList<OutEvent>, t: Long) {
    if (hovering) out += mouse(MotionEvent.ACTION_HOVER_EXIT, 0, 0, t, keyboard.metaState)
    hovering = false
  }

  private fun enterHover(out: MutableList<OutEvent>, t: Long) {
    out += mouse(MotionEvent.ACTION_HOVER_MOVE, 0, 0, t, keyboard.metaState)
    hovering = true
  }

  // Two-finger scrolling uses one fake finger that starts at the cursor and moves with the
  // content, so it moves against the scroll (a scroll down drags content up).
  private fun scroll(out: MutableList<OutEvent>, t: Long, dx: Float, dy: Float) {
    if (gesture != Gesture.SCROLL) {
      if (gesture != Gesture.NONE || !gestureAllowed()) return
      exitHover(out, t)
      start(Gesture.SCROLL, t, 1)
      fingerX[0] = x
      fingerY[0] = y
      flingMayBeInProgress = false
      out += fakeFingers(MotionEvent.ACTION_DOWN, t, 1)
    }
    fingerX[0] -= dx
    fingerY[0] -= dy
    out +=
      fakeFingers(
        MotionEvent.ACTION_MOVE,
        t,
        1,
        mapOf(
          MotionEvent.AXIS_GESTURE_SCROLL_X_DISTANCE to dx,
          MotionEvent.AXIS_GESTURE_SCROLL_Y_DISTANCE to dy,
        ),
      )
  }

  // Fingers on the touchpad after a scroll may mean "stop the fling", so GestureConverter's
  // FLING_TAP_DOWN sends a fake finger down and cancels it.
  private fun holdBegin(out: MutableList<OutEvent>, t: Long) {
    if (gesture != Gesture.NONE || !flingMayBeInProgress || !gestureAllowed()) return
    flingMayBeInProgress = false
    exitHover(out, t)
    start(Gesture.SCROLL, t, 1)
    fingerX[0] = x
    fingerY[0] = y
    out += fakeFingers(MotionEvent.ACTION_DOWN, t, 1)
    out += fakeFingers(MotionEvent.ACTION_CANCEL, t, 1)
    gesture = Gesture.NONE
    enterHover(out, t)
  }

  private fun pinchBegin(out: MutableList<OutEvent>, t: Long) {
    endGesture(out, t)
    if (!gestureAllowed()) return
    exitHover(out, t)
    start(Gesture.PINCH, t, 2)
    pinchScale = 1.0
    pinchAngle = 0.0
    pinchX = x
    pinchY = y
    placePinchFingers()
    out += fakeFingers(MotionEvent.ACTION_DOWN, t, 1, pinchFactor(1f))
    out += fakeFingers(pointerAction(MotionEvent.ACTION_POINTER_DOWN, 1), t, 2, pinchFactor(1f))
  }

  // Wayland's scale is relative to the begin, and Android's factor to the last event. The center
  // follows the fingers' common motion, and rotation turns the fingers around it.
  private fun pinchUpdate(out: MutableList<OutEvent>, t: Long, e: HostEvent.PinchUpdate) {
    if (gesture != Gesture.PINCH) return
    val scale = e.scale.coerceIn(MIN_PINCH_SCALE, MAX_PINCH_SCALE)
    val factor = (scale / pinchScale).toFloat()
    pinchScale = scale
    pinchAngle += Math.toRadians(e.rotation)
    pinchX += e.dx
    pinchY += e.dy
    placePinchFingers()
    out += fakeFingers(MotionEvent.ACTION_MOVE, t, 2, pinchFactor(factor))
  }

  private fun placePinchFingers() {
    val half = INITIAL_PINCH_SEPARATION_PX * pinchScale / 2
    val hx = (half * Math.cos(pinchAngle)).toFloat()
    val hy = (half * Math.sin(pinchAngle)).toFloat()
    fingerX[0] = pinchX - hx
    fingerY[0] = pinchY - hy
    fingerX[1] = pinchX + hx
    fingerY[1] = pinchY + hy
  }

  private fun pinchFactor(f: Float) = mapOf(MotionEvent.AXIS_GESTURE_PINCH_SCALE_FACTOR to f)

  // Swipes of three or more fingers use fake fingers 100 px apart, centered on the cursor.
  private fun swipeBegin(out: MutableList<OutEvent>, t: Long, fingers: Int) {
    endGesture(out, t)
    if (!gestureAllowed()) return
    exitHover(out, t)
    val n = fingers.coerceIn(2, MAX_SWIPE_FINGERS)
    start(Gesture.SWIPE, t, n)
    for (i in 0 until n) {
      fingerX[i] = x + SWIPE_FINGER_SPACING_PX * (i - (n - 1) / 2f)
      fingerY[i] = y
    }
    out += fakeFingers(MotionEvent.ACTION_DOWN, t, 1, swipeAxes(0f, 0f))
    for (i in 1 until n) {
      out +=
        fakeFingers(pointerAction(MotionEvent.ACTION_POINTER_DOWN, i), t, i + 1, swipeAxes(0f, 0f))
    }
  }

  private fun swipeUpdate(out: MutableList<OutEvent>, t: Long, dx: Float, dy: Float) {
    if (gesture != Gesture.SWIPE) return
    for (i in 0 until gestureFingers) {
      fingerX[i] += dx
      fingerY[i] += dy
    }
    // GestureConverter normalizes by the touchpad's size, and the window's size is the closest
    // available stand-in.
    val w = windows[pointerDisplay]
    val width = w?.let { (it.width * it.scale).toFloat() }?.takeIf { it > 0f } ?: 1000f
    val height = w?.let { (it.height * it.scale).toFloat() }?.takeIf { it > 0f } ?: 1000f
    out +=
      fakeFingers(MotionEvent.ACTION_MOVE, t, gestureFingers, swipeAxes(dx / width, dy / height))
  }

  private fun swipeAxes(xOffset: Float, yOffset: Float) =
    mapOf(
      AXIS_GESTURE_SWIPE_FINGER_COUNT to gestureFingers.toFloat(),
      MotionEvent.AXIS_GESTURE_X_OFFSET to xOffset,
      MotionEvent.AXIS_GESTURE_Y_OFFSET to yOffset,
    )

  private fun gestureEnd(out: MutableList<OutEvent>, t: Long, kind: Gesture, cancelled: Boolean) {
    if (gesture != kind) return
    if (cancelled) {
      out += fakeFingers(MotionEvent.ACTION_CANCEL, t, gestureFingers)
      gesture = Gesture.NONE
      enterHover(out, t)
    } else {
      endGesture(out, t)
    }
  }

  /** Lifts the fake fingers of the gesture in progress, then hovers again (unless leaving). */
  private fun endGesture(out: MutableList<OutEvent>, t: Long) {
    when (gesture) {
      Gesture.NONE -> return
      Gesture.SCROLL -> {
        out += fakeFingers(MotionEvent.ACTION_UP, t, 1)
        // The app may fling now; the next hold stops it.
        flingMayBeInProgress = true
      }
      Gesture.PINCH -> {
        out += fakeFingers(pointerAction(MotionEvent.ACTION_POINTER_UP, 1), t, 2, pinchFactor(1f))
        out += fakeFingers(MotionEvent.ACTION_UP, t, 1, pinchFactor(1f))
      }
      Gesture.SWIPE -> {
        for (i in gestureFingers - 1 downTo 1) {
          out +=
            fakeFingers(
              pointerAction(MotionEvent.ACTION_POINTER_UP, i),
              t,
              i + 1,
              swipeAxes(0f, 0f),
            )
        }
        out += fakeFingers(MotionEvent.ACTION_UP, t, 1, swipeAxes(0f, 0f))
      }
    }
    gesture = Gesture.NONE
    if (!left) enterHover(out, t)
  }

  private fun start(kind: Gesture, t: Long, fingers: Int) {
    gesture = kind
    gestureDownTime = t
    gestureFingers = fingers
  }

  // Like GestureConverter::makeMotionArgs, this uses the mouse source, finger tools and the
  // gesture's classification. Gestures don't move the focus, and scrolls are marked as generated.
  private fun fakeFingers(
    action: Int,
    t: Long,
    n: Int,
    axes: Map<Int, Float> = emptyMap(),
  ): OutMotion {
    val classification =
      when (gesture) {
        Gesture.NONE -> MotionEvent.CLASSIFICATION_NONE
        Gesture.SCROLL -> MotionEvent.CLASSIFICATION_TWO_FINGER_SWIPE
        Gesture.PINCH -> MotionEvent.CLASSIFICATION_PINCH
        Gesture.SWIPE -> CLASSIFICATION_MULTI_FINGER_SWIPE
      }
    var flags = FLAG_NO_FOCUS_CHANGE
    if (gesture == Gesture.SCROLL) flags = flags or FLAG_IS_GENERATED_GESTURE
    return OutMotion(
      source = InputDevice.SOURCE_MOUSE,
      action = action,
      actionButton = 0,
      buttonState = 0,
      metaState = keyboard.metaState,
      downTime = gestureDownTime,
      eventTime = t,
      pointers =
        (0 until n).map {
          OutPointer(
            it,
            MotionEvent.TOOL_TYPE_FINGER,
            fingerX[it],
            fingerY[it],
            1f,
            axes = if (it == 0) axes else emptyMap(),
          )
        },
      displayId = pointerDisplay,
      classification = classification,
      flags = flags,
    )
  }

  // --- pen -----------------------------------------------------------------
  // TouchInputMapper's stylus: HOVER_ENTER/MOVE/EXIT in proximity, DOWN/MOVE/UP on contact,
  // stylus buttons as BUTTON_PRESS/RELEASE, in its sync order.

  private fun penFrame(out: MutableList<OutEvent>) {
    val t = if (penFrameTime >= 0) clock.current() else clock.untimed()
    val enter = penEnter
    val leave = penLeave
    val tip = penTip
    var changed = penChanged
    val buttonChanges = penButtonChanges.toList()
    clearPenFrame()

    if (enter != null) {
      if (penIn) penFrameOut(out, t)
      penIgnored = false
      if (buttons != 0 || touches.isNotEmpty() || gesture != Gesture.NONE) {
        penIgnored = true
      } else {
        // The pen takes the stream over from the mouse.
        exitHover(out, t)
        penIn = true
        penTool = PEN_TOOLS[enter.tool] ?: MotionEvent.TOOL_TYPE_STYLUS
        penDisplay = enter.display
        changed = true
      }
    }
    if (penIgnored) {
      if (leave) penIgnored = false
      return
    }
    if (!penIn) return

    var newButtons = penButtons
    for ((code, pressed) in buttonChanges) {
      val b = STYLUS_BUTTONS[code] ?: continue
      newButtons = if (pressed) newButtons or b else newButtons and b.inv()
    }
    if (leave) newButtons = 0
    val down = !leave && (tip ?: penDown)

    // TouchInputMapper::sync: button releases, hover exit, touches, hover enter and move, presses.
    for (b in STYLUS_BUTTON_ORDER) {
      if (penButtons and b != 0 && newButtons and b == 0) {
        penButtons = penButtons and b.inv()
        out += pen(MotionEvent.ACTION_BUTTON_RELEASE, b, t)
      }
    }
    if (penHovering && (down || leave)) {
      out += pen(MotionEvent.ACTION_HOVER_EXIT, 0, t)
      penHovering = false
    }
    if (!penDown && down) {
      penDownTime = t
      penDown = true
      out += pen(MotionEvent.ACTION_DOWN, 0, t)
    } else if (penDown && !down) {
      // As in TouchInputMapper, the lift repeats the last contact, and this frame's position and
      // the usual zero pressure go to the hover that follows.
      out += pen(MotionEvent.ACTION_UP, 0, t)
      penDown = false
      changed = true
    } else if (penDown && changed) {
      out += pen(MotionEvent.ACTION_MOVE, 0, t)
    }
    if (!down && !leave) {
      if (!penHovering) {
        out += pen(MotionEvent.ACTION_HOVER_ENTER, 0, t)
        penHovering = true
        changed = true
      }
      if (changed) out += pen(MotionEvent.ACTION_HOVER_MOVE, 0, t)
    }
    for (b in STYLUS_BUTTON_ORDER) {
      if (penButtons and b == 0 && newButtons and b != 0) {
        penButtons = penButtons or b
        out += pen(MotionEvent.ACTION_BUTTON_PRESS, b, t)
      }
    }
    if (leave) penFrameOut(out, t)
  }

  // When the tool leaves or another comes in, lift, release and exit what is still held.
  private fun penFrameOut(out: MutableList<OutEvent>, t: Long) {
    for (b in STYLUS_BUTTON_ORDER) {
      if (penButtons and b != 0) {
        penButtons = penButtons and b.inv()
        out += pen(MotionEvent.ACTION_BUTTON_RELEASE, b, t)
      }
    }
    if (penDown) out += pen(MotionEvent.ACTION_UP, 0, t)
    penLast = null
    if (penHovering) out += pen(MotionEvent.ACTION_HOVER_EXIT, 0, t)
    penIn = false
    penDown = false
    penHovering = false
    penPressure = 0f
    penDistance = -1f
    penTiltX = Double.NaN
  }

  private fun clearPenFrame() {
    penFrameTime = -1
    penEnter = null
    penLeave = false
    penTip = null
    penChanged = false
    penButtonChanges.clear()
  }

  private fun pen(action: Int, actionButton: Int, t: Long): OutMotion {
    val hover =
      when (action) {
        MotionEvent.ACTION_HOVER_ENTER,
        MotionEvent.ACTION_HOVER_MOVE,
        MotionEvent.ACTION_HOVER_EXIT -> true
        MotionEvent.ACTION_DOWN,
        MotionEvent.ACTION_MOVE,
        MotionEvent.ACTION_UP -> false
        else -> !penDown
      }
    val axes = HashMap<Int, Float>(4)
    if (!penTiltX.isNaN()) {
      // As in TouchInputMapper, report the tilt from the perpendicular and the direction it leans.
      val tx = Math.toRadians(penTiltX)
      val ty = Math.toRadians(penTiltY)
      axes[MotionEvent.AXIS_TILT] = Math.acos(Math.cos(tx) * Math.cos(ty)).toFloat()
      axes[MotionEvent.AXIS_ORIENTATION] = Math.atan2(-Math.sin(tx), Math.sin(ty)).toFloat()
    }
    if (hover && penDistance >= 0f) axes[MotionEvent.AXIS_DISTANCE] = penDistance
    val last = penLast
    val pointer =
      if (action == MotionEvent.ACTION_UP && last != null) last
      else OutPointer(0, penTool, penX, penY, if (hover) 0f else penPressure, axes = axes)
    if (action == MotionEvent.ACTION_DOWN || action == MotionEvent.ACTION_MOVE) penLast = pointer
    return OutMotion(
      source = InputDevice.SOURCE_STYLUS or InputDevice.SOURCE_TOUCHSCREEN,
      action = action,
      actionButton = actionButton,
      buttonState = penButtons,
      metaState = keyboard.metaState,
      downTime = penDownTime,
      eventTime = t,
      pointers = listOf(pointer),
      displayId = penDisplay,
    )
  }

  // --- touch ---------------------------------------------------------------

  private fun touchFrame(out: MutableList<OutEvent>) {
    val t = if (touchFrameTime >= 0) clock.current() else clock.untimed()
    val meta = keyboard.metaState
    val downs = touchDowns.toList()
    val moves = touchMoves.values.toList()
    val ups = touchUps.toList()
    clearTouchFrame()

    // Moves of existing touches go in one ACTION_MOVE with every pointer.
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
      // Drop a touch sequence that starts during a mouse drag, pointers beyond the limit and
      // fingers on another window than the sequence's.
      if (
        (touches.isEmpty() && (buttons != 0 || gesture != Gesture.NONE || penIn)) ||
          touches.size >= MAX_POINTERS ||
          (touches.isNotEmpty() && d.display != touchDisplay)
      ) {
        suppressedTouches += d.id
        continue
      }
      if (touches.isEmpty()) {
        if (hovering) out += mouse(MotionEvent.ACTION_HOVER_EXIT, 0, 0, t, meta)
        hovering = false
        touchDownTime = t
        touchDisplay = d.display
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
      displayId = touchDisplay,
    )

  private fun pointerAction(action: Int, index: Int) = action or (index shl INDEX_SHIFT)

  internal companion object {
    const val INDEX_SHIFT = MotionEvent.ACTION_POINTER_INDEX_SHIFT
    const val MAX_POINTERS = 16 // MAX_POINTERS in Android's Input.h

    // wl_pointer.axis_source
    const val AXIS_SOURCE_FINGER = 1

    // GestureConverter
    const val INITIAL_PINCH_SEPARATION_PX = 200.0
    const val SWIPE_FINGER_SPACING_PX = 100f
    const val MAX_SWIPE_FINGERS = 5
    const val MIN_PINCH_SCALE = 0.01
    const val MAX_PINCH_SCALE = 100.0

    // Hidden MotionEvent constants.
    const val FLAG_IS_GENERATED_GESTURE = 0x8
    const val FLAG_NO_FOCUS_CHANGE = 0x40
    const val AXIS_GESTURE_SWIPE_FINGER_COUNT = 53
    const val CLASSIFICATION_MULTI_FINGER_SWIPE = 4

    // linux/input-event-codes.h: tablet tools and stylus buttons.
    const val BTN_TOOL_RUBBER = 0x141
    const val BTN_TOOL_FINGER = 0x145
    const val BTN_TOOL_MOUSE = 0x146
    const val BTN_TOOL_LENS = 0x147
    const val BTN_STYLUS = 0x14b
    const val BTN_STYLUS2 = 0x14c

    val PEN_TOOLS =
      mapOf(
        BTN_TOOL_RUBBER to MotionEvent.TOOL_TYPE_ERASER,
        BTN_TOOL_FINGER to MotionEvent.TOOL_TYPE_FINGER,
        BTN_TOOL_MOUSE to MotionEvent.TOOL_TYPE_MOUSE,
        BTN_TOOL_LENS to MotionEvent.TOOL_TYPE_MOUSE,
      )
    val STYLUS_BUTTONS =
      mapOf(
        BTN_STYLUS to MotionEvent.BUTTON_STYLUS_PRIMARY,
        BTN_STYLUS2 to MotionEvent.BUTTON_STYLUS_SECONDARY,
      )
    val STYLUS_BUTTON_ORDER =
      intArrayOf(MotionEvent.BUTTON_STYLUS_PRIMARY, MotionEvent.BUTTON_STYLUS_SECONDARY)

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
