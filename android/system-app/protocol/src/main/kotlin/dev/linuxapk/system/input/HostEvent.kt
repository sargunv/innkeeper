// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.input

/**
 * An event from the bridge's input service (host/bridge/src/input/proto.rs): one input line
 * (protocol v2, [parse]), or a window's mapping. Coordinates are in the pixels of the display the
 * event targets, times are the compositor's millisecond clock (u32, wraps), buttons and keys are
 * Linux evdev codes.
 *
 * Every window is an Android display: the lines that start a pointer, keyboard or touch sequence
 * (and `window`, `close` and the leave lines) carry the display id of their window.
 */
sealed interface HostEvent {
  /** Window size in logical (surface) pixels and the output scale. */
  data class Window(val width: Int, val height: Int, val scale: Double, val display: Int = 0) :
    HostEvent

  /** The pointer entered the window of [display]; later pointer events go there. */
  data class PointerEnter(val x: Float, val y: Float, val display: Int = 0) : HostEvent

  data class PointerLeave(val display: Int = -1) : HostEvent

  data class PointerMotion(val time: Long, val x: Float, val y: Float) : HostEvent

  data class PointerButton(val time: Long, val button: Int, val pressed: Boolean) : HostEvent

  /** [axis] 0 = vertical, 1 = horizontal; [value] in surface pixels, positive = down/right. */
  data class PointerAxis(val time: Long, val axis: Int, val value: Double) : HostEvent

  /** High-resolution wheel steps: 120 = one detent. */
  data class PointerAxisValue120(val axis: Int, val v120: Int) : HostEvent

  data class PointerAxisStop(val time: Long, val axis: Int) : HostEvent

  data object PointerFrame : HostEvent

  /** The window of [display] got the keyboard focus; later keys go there. */
  data class KeyboardEnter(val display: Int = 0) : HostEvent

  data class KeyboardLeave(val display: Int = -1) : HostEvent

  data class Key(val time: Long, val code: Int, val pressed: Boolean) : HostEvent

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

  /** The user asked to close the window of [display]. */
  data class Close(val display: Int = 0) : HostEvent

  // The input service's window events (not lines), see WindowManagerFeature.

  /** [display] shows in the window the window manager asked for. */
  data class WindowMapped(val display: Int) : HostEvent

  /** The window of [display] is gone: its HWC display went away, or it was destroyed. */
  data class WindowUnmapped(val display: Int) : HostEvent

  // Text input (docs/input.md): the host input method's results for the Linux.apk
  // IME, gathered until [TextDone]. Lengths and offsets are UTF-8 bytes; text is decoded.

  /** Delete [before] bytes before the cursor and [after] bytes after it. */
  data class TextDelete(val before: Int, val after: Int) : HostEvent

  /** Insert [text] (several lines append). */
  data class TextCommit(val text: String) : HostEvent

  /** The new preedit and its cursor (-1: hidden); empty: none. */
  data class TextPreedit(val begin: Int, val end: Int, val text: String) : HostEvent

  /** Apply what came since the last one, for the window of [display]. */
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

      fun bool(i: Int) =
        when (f[i]) {
          "1" -> true
          "0" -> false
          else -> throw NumberFormatException(f[i])
        }

      val args = f.size - 1

      // Trailing display id; [default] if absent.
      fun display(i: Int, default: Int) = if (args >= i) int(i) else default

      return when (f[0]) {
        "window" -> need(args, 3) { Window(int(1), int(2), f[3].toDouble(), display(4, 0)) }
        "pointer_enter" -> need(args, 2) { PointerEnter(float(1), float(2), display(3, 0)) }
        "pointer_leave" -> PointerLeave(display(1, -1))
        "pointer_motion" -> need(args, 3) { PointerMotion(time(1), float(2), float(3)) }
        "pointer_button" -> need(args, 3) { PointerButton(time(1), int(2), bool(3)) }
        "pointer_axis" -> need(args, 3) { PointerAxis(time(1), int(2), f[3].toDouble()) }
        "pointer_axis_value120" -> need(args, 2) { PointerAxisValue120(int(1), int(2)) }
        "pointer_axis_stop" -> need(args, 2) { PointerAxisStop(time(1), int(2)) }
        "pointer_frame" -> PointerFrame
        "keyboard_enter" -> KeyboardEnter(display(1, 0))
        "keyboard_leave" -> KeyboardLeave(display(1, -1))
        "key" -> need(args, 3) { Key(time(1), int(2), bool(3)) }
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

    // Extra trailing fields are accepted (forward compatibility); missing ones are not.
    private inline fun need(args: Int, n: Int, make: () -> HostEvent): HostEvent? =
      if (args >= n) make() else null
  }
}
