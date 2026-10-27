// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.input

/**
 * One line of the composer's input stream (`/dev/socket/linuxapk-input`, protocol version 2; see
 * docs/hal-display.md, "Input event stream"). Coordinates are in the pixels of the display the
 * event targets, times are the compositor's millisecond clock (u32, wraps), buttons and keys are
 * Linux evdev codes.
 *
 * Version 2 adds the Android display id of the window to the lines that start a pointer, keyboard
 * or touch sequence (and to `window`, `close` and the leave lines), plus the window-manager replies
 * (`window_pending`, ...). Version 1 lines have no display id; it defaults to display 0, which is
 * what version 1 meant.
 */
sealed interface HostEvent {
  data class Hello(val version: Int, val displayWidth: Int, val displayHeight: Int) : HostEvent

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

  // Window manager replies (v2), see WindowManagerFeature.

  /** A `window_new` request was queued; the virtual display may now get its surface. */
  data class WindowPending(val display: Int) : HostEvent

  /** The window for [display] exists (composer display [hwcDisplay]). */
  data class WindowMapped(val display: Int, val hwcDisplay: Long) : HostEvent

  /** The window for [display] was destroyed. */
  data class WindowGone(val display: Int) : HostEvent

  data class WindowFailed(val display: Int, val reason: String) : HostEvent

  data class Pong(val token: String) : HostEvent

  companion object {
    const val PROTOCOL_VERSION = 2

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

      // Optional trailing display id (v2).
      fun display(i: Int, default: Int) = if (args >= i) int(i) else default

      return when (f[0]) {
        "hello" -> need(args, 3) { Hello(int(1), int(2), int(3)) }
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
        "window_pending" -> need(args, 1) { WindowPending(int(1)) }
        "window_mapped" -> need(args, 1) { WindowMapped(int(1), if (args >= 2) time(2) else -1) }
        "window_gone" -> need(args, 1) { WindowGone(int(1)) }
        "window_failed" ->
          need(args, 1) { WindowFailed(int(1), f.drop(2).joinToString(" ").ifEmpty { "?" }) }
        "pong" -> Pong(f.drop(1).joinToString(" "))
        else -> null
      }
    }

    // Extra trailing fields are accepted (forward compatibility); missing ones are not.
    private inline fun need(args: Int, n: Int, make: () -> HostEvent): HostEvent? =
      if (args >= n) make() else null
  }
}
