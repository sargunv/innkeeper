// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.input

/**
 * An event from the bridge's input service (host/bridge/src/input/proto.rs), either one input line
 * (protocol v2, [parse]) or a window's mapping. Coordinates are in the pixels of the display the
 * event targets, times are the compositor's millisecond clock (u32, wraps), buttons and keys are
 * Linux evdev codes.
 *
 * Every window is an Android display. The lines that start a pointer, keyboard or touch sequence,
 * as well as `window`, `close` and the leave lines, carry the display id of their window.
 */
sealed interface HostEvent {
  /** Window size in logical (surface) pixels and the output scale. */
  data class Window(val width: Int, val height: Int, val scale: Double, val display: Int = 0) :
    HostEvent

  /** Visible output IDs and current mode refresh in mHz; zero is an unknown rate. */
  data class WindowOutputs(val outputs: Map<Int, Int>, val display: Int) : HostEvent

  data class WindowVisible(val visible: Boolean, val display: Int) : HostEvent

  /** The pointer entered the window of [display]; later pointer events go there. */
  data class PointerEnter(val x: Float, val y: Float, val display: Int = 0) : HostEvent

  data class PointerLeave(val display: Int = -1) : HostEvent

  data class PointerRelative(val display: Int, val dx: Float, val dy: Float) : HostEvent

  data class PointerMotion(val time: Long, val x: Float, val y: Float) : HostEvent

  data class PointerButton(val time: Long, val button: Int, val pressed: Boolean) : HostEvent

  /**
   * [axis] is 0 for vertical, 1 for horizontal. [value] is in surface pixels, positive down/right.
   */
  data class PointerAxis(val time: Long, val axis: Int, val value: Double) : HostEvent

  /** High-resolution wheel steps, where 120 is one detent. */
  data class PointerAxisValue120(val axis: Int, val v120: Int) : HostEvent

  data class PointerAxisStop(val time: Long, val axis: Int) : HostEvent

  /** What the frame's axis events come from: 0 wheel, 1 finger, 2 continuous, 3 wheel tilt. */
  data class PointerAxisSource(val source: Int) : HostEvent

  data object PointerFrame : HostEvent

  // Touchpad gestures, where the pointer is. Deltas are in display pixels.

  data class SwipeBegin(val time: Long, val fingers: Int) : HostEvent

  data class SwipeUpdate(val time: Long, val dx: Float, val dy: Float) : HostEvent

  data class SwipeEnd(val time: Long, val cancelled: Boolean) : HostEvent

  data class PinchBegin(val time: Long, val fingers: Int) : HostEvent

  /** [scale] is relative to the begin. [rotation] is in degrees clockwise since the last update. */
  data class PinchUpdate(
    val time: Long,
    val dx: Float,
    val dy: Float,
    val scale: Double,
    val rotation: Double,
  ) : HostEvent

  data class PinchEnd(val time: Long, val cancelled: Boolean) : HostEvent

  data class HoldBegin(val time: Long, val fingers: Int) : HostEvent

  data class HoldEnd(val time: Long, val cancelled: Boolean) : HostEvent

  // A tablet tool (pen, eraser), gathered until [TabletFrame].

  /** [tool] is its `BTN_TOOL_*` code. Later tablet events go to [display]. */
  data class TabletProximityIn(val tool: Int, val display: Int = 0) : HostEvent

  data object TabletProximityOut : HostEvent

  data object TabletDown : HostEvent

  data object TabletUp : HostEvent

  data class TabletMotion(val x: Float, val y: Float) : HostEvent

  /** 0..65535. */
  data class TabletPressure(val pressure: Int) : HostEvent

  /** 0..65535. */
  data class TabletDistance(val distance: Int) : HostEvent

  /** Degrees from the perpendicular along each axis. */
  data class TabletTilt(val x: Double, val y: Double) : HostEvent

  data class TabletButton(val button: Int, val pressed: Boolean) : HostEvent

  data class TabletFrame(val time: Long) : HostEvent

  /** The window of [display] got the keyboard focus; later keys go there. */
  data class KeyboardEnter(val display: Int = 0) : HostEvent

  data class KeyboardLeave(val display: Int = -1) : HostEvent

  /**
   * Key [code] (evdev). Under the host's keymap a press also carries [sym], the key's level-1
   * keysym in the active layout (0 if unknown), and what it types, which is either [text] or
   * [compose] for a step of a compose sequence (a dead key) that types nothing. [repeat] means the
   * compositor repeated a held key (wl_keyboard v10, after a repeat rate of 0).
   */
  data class Key(
    val time: Long,
    val code: Int,
    val pressed: Boolean,
    val sym: Int = 0,
    val text: String? = null,
    val compose: Boolean = false,
    val repeat: Boolean = false,
  ) : HostEvent

  /** The host's key repeat of [rate] per second, or 0 for off, after [delay] ms. */
  data class RepeatInfo(val rate: Int, val delay: Int) : HostEvent

  /** xkb modifier masks in the compositor's keymap. */
  data class Modifiers(val depressed: Int, val latched: Int, val locked: Int, val group: Int) :
    HostEvent

  data class TouchDown(
    val time: Long,
    val id: Int,
    val x: Float,
    val y: Float,
    val display: Int = 0,
  ) : HostEvent

  data class TouchMotion(val time: Long, val id: Int, val x: Float, val y: Float) : HostEvent

  data class TouchUp(val time: Long, val id: Int) : HostEvent

  data object TouchFrame : HostEvent

  data object TouchCancel : HostEvent

  /** A title-bar Back click, independent of the keyboard and pointer focus. */
  data class Back(val display: Int) : HostEvent

  /** The user asked to close the window of [display]. */
  data class Close(val display: Int = 0) : HostEvent

  // The input service's window events, which are not lines. See WindowManagerFeature.

  /** [display] shows in the window the window manager asked for. */
  data class WindowMapped(val display: Int) : HostEvent

  /** The window of [display] is gone because its HWC display went away or it was destroyed. */
  data class WindowUnmapped(val display: Int) : HostEvent

  // Text input (docs/input.md): the host input method's results for the Innkeeper
  // IME, gathered until [TextDone]. Lengths and offsets are UTF-8 bytes; text is decoded.

  /** Delete [before] bytes before the cursor and [after] bytes after it. */
  data class TextDelete(val before: Int, val after: Int) : HostEvent

  /** Insert [text]. Several of these lines append to each other. */
  data class TextCommit(val text: String) : HostEvent

  /** The new preedit and its cursor, which is -1 when hidden. An empty preedit means none. */
  data class TextPreedit(val begin: Int, val end: Int, val text: String) : HostEvent

  /** Apply what arrived since the last TextDone to the window of [display]. */
  data class TextDone(val display: Int) : HostEvent

  companion object {
    /** Parses one line (without the newline). Returns null for unknown or malformed lines. */
    fun parse(line: String): HostEvent? {
      val f = line.trim().split(' ').filter { it.isNotEmpty() }
      if (f.isEmpty()) return null
      return try {
        parseFields(f)
      } catch (_: NumberFormatException) {
        null
      } catch (_: IndexOutOfBoundsException) {
        null
      }
    }

    private fun parseFields(f: List<String>): HostEvent? {
      fun int(i: Int) = f[i].toInt()

      fun time(i: Int) = f[i].toLong()

      fun float(i: Int) = f[i].toFloat()

      fun bool(i: Int) = flag(f[i])

      val args = f.size - 1

      // The trailing display id, or [default] if absent.
      fun display(i: Int, default: Int) = if (args >= i) int(i) else default

      return when (f[0]) {
        "window_outputs" ->
          if (args < 1 || args % 2 == 0) null
          else WindowOutputs((2..args step 2).associate { int(it) to int(it + 1) }, int(1))
        "window_visible" -> need(args, 2) { WindowVisible(bool(1), int(2)) }
        "window" -> need(args, 3) { Window(int(1), int(2), f[3].toDouble(), display(4, 0)) }
        "pointer_enter" -> need(args, 2) { PointerEnter(float(1), float(2), display(3, 0)) }
        "pointer_leave" -> PointerLeave(display(1, -1))
        "pointer_relative" -> need(args, 3) { PointerRelative(int(1), float(2), float(3)) }
        "pointer_motion" -> need(args, 3) { PointerMotion(time(1), float(2), float(3)) }
        "pointer_button" -> need(args, 3) { PointerButton(time(1), int(2), bool(3)) }
        "pointer_axis" -> need(args, 3) { PointerAxis(time(1), int(2), f[3].toDouble()) }
        "pointer_axis_value120" -> need(args, 2) { PointerAxisValue120(int(1), int(2)) }
        "pointer_axis_stop" -> need(args, 2) { PointerAxisStop(time(1), int(2)) }
        "pointer_axis_source" -> need(args, 1) { PointerAxisSource(int(1)) }
        "pointer_frame" -> PointerFrame
        "gesture_swipe_begin" -> need(args, 2) { SwipeBegin(time(1), int(2)) }
        "gesture_swipe_update" -> need(args, 3) { SwipeUpdate(time(1), float(2), float(3)) }
        "gesture_swipe_end" -> need(args, 2) { SwipeEnd(time(1), bool(2)) }
        "gesture_pinch_begin" -> need(args, 2) { PinchBegin(time(1), int(2)) }
        "gesture_pinch_update" ->
          need(args, 5) {
            PinchUpdate(time(1), float(2), float(3), f[4].toDouble(), f[5].toDouble())
          }
        "gesture_pinch_end" -> need(args, 2) { PinchEnd(time(1), bool(2)) }
        "gesture_hold_begin" -> need(args, 2) { HoldBegin(time(1), int(2)) }
        "gesture_hold_end" -> need(args, 2) { HoldEnd(time(1), bool(2)) }
        "tablet_proximity_in" -> need(args, 1) { TabletProximityIn(int(1), display(2, 0)) }
        "tablet_proximity_out" -> TabletProximityOut
        "tablet_down" -> TabletDown
        "tablet_up" -> TabletUp
        "tablet_motion" -> need(args, 2) { TabletMotion(float(1), float(2)) }
        "tablet_pressure" -> need(args, 1) { TabletPressure(int(1)) }
        "tablet_distance" -> need(args, 1) { TabletDistance(int(1)) }
        "tablet_tilt" -> need(args, 2) { TabletTilt(f[1].toDouble(), f[2].toDouble()) }
        "tablet_button" -> need(args, 2) { TabletButton(int(1), bool(2)) }
        "tablet_frame" -> need(args, 1) { TabletFrame(time(1)) }
        "keyboard_enter" -> KeyboardEnter(display(1, 0))
        "keyboard_leave" -> KeyboardLeave(display(1, -1))
        "key" -> need(args, 3) { key(f) ?: return null }
        "repeat_info" -> need(args, 2) { RepeatInfo(int(1), int(2)) }
        "modifiers" ->
          need(args, 4) {
            Modifiers(
              f[1].toLong().toInt(),
              f[2].toLong().toInt(),
              f[3].toLong().toInt(),
              f[4].toLong().toInt(),
            )
          }
        "touch_down" ->
          need(args, 4) { TouchDown(time(1), int(2), float(3), float(4), display(5, 0)) }
        "touch_motion" -> need(args, 4) { TouchMotion(time(1), int(2), float(3), float(4)) }
        "touch_up" -> need(args, 2) { TouchUp(time(1), int(2)) }
        "touch_frame" -> TouchFrame
        "touch_cancel" -> TouchCancel
        "back" -> need(args, 1) { Back(int(1)) }
        "close" -> Close(display(1, 0))
        "ti_delete" -> need(args, 2) { TextDelete(int(1), int(2)) }
        "ti_commit" -> TextCommit(PercentText.decode(f.getOrElse(1) { "" }) ?: return null)
        "ti_preedit" ->
          need(args, 2) {
            TextPreedit(int(1), int(2), PercentText.decode(f.getOrElse(3) { "" }) ?: return null)
          }
        "ti_done" -> need(args, 1) { TextDone(int(1)) }
        else -> null
      }
    }

    // `key <t> <code> <0|1|2> [<sym> [text <text> | compose]]`. Null if the text is malformed.
    private fun key(f: List<String>): Key? {
      val repeat = f[3] == "2"
      val pressed = repeat || flag(f[3])
      val sym = f.getOrNull(4)?.toLong()?.toInt() ?: 0
      val k = Key(f[1].toLong(), f[2].toInt(), pressed, sym, repeat = repeat)
      return when (f.getOrNull(5)) {
        "text" -> k.copy(text = PercentText.decode(f.getOrElse(6) { "" }) ?: return null)
        "compose" -> k.copy(compose = true)
        else -> k
      }
    }

    private fun flag(s: String) =
      when (s) {
        "1" -> true
        "0" -> false
        else -> throw NumberFormatException(s)
      }

    // Extra trailing fields are accepted for forward compatibility, but missing ones are not.
    private inline fun need(args: Int, n: Int, make: () -> HostEvent): HostEvent? =
      if (args >= n) make() else null
  }
}
