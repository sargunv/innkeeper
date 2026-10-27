// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.input

/**
 * One line of the composer's input stream (`/dev/socket/linuxapk-input`, protocol version 1; see
 * docs/hal-display.md, "Input event stream"). Coordinates are in display pixels, times are the
 * compositor's millisecond clock (u32, wraps), buttons and keys are Linux evdev codes.
 */
sealed interface HostEvent {
  data class Hello(val version: Int, val displayWidth: Int, val displayHeight: Int) : HostEvent

  /** Window size in logical (surface) pixels and the output scale. */
  data class Window(val width: Int, val height: Int, val scale: Double) : HostEvent

  data class PointerEnter(val x: Float, val y: Float) : HostEvent

  data object PointerLeave : HostEvent

  data class PointerMotion(val time: Long, val x: Float, val y: Float) : HostEvent

  data class PointerButton(val time: Long, val button: Int, val pressed: Boolean) : HostEvent

  /** [axis] 0 = vertical, 1 = horizontal; [value] in surface pixels, positive = down/right. */
  data class PointerAxis(val time: Long, val axis: Int, val value: Double) : HostEvent

  /** High-resolution wheel steps: 120 = one detent. */
  data class PointerAxisValue120(val axis: Int, val v120: Int) : HostEvent

  data class PointerAxisStop(val time: Long, val axis: Int) : HostEvent

  data object PointerFrame : HostEvent

  data object KeyboardEnter : HostEvent

  data object KeyboardLeave : HostEvent

  data class Key(val time: Long, val code: Int, val pressed: Boolean) : HostEvent

  /** xkb modifier masks in the compositor's keymap. */
  data class Modifiers(val depressed: Int, val latched: Int, val locked: Int, val group: Int) :
    HostEvent

  data class TouchDown(val time: Long, val id: Int, val x: Float, val y: Float) : HostEvent

  data class TouchMotion(val time: Long, val id: Int, val x: Float, val y: Float) : HostEvent

  data class TouchUp(val time: Long, val id: Int) : HostEvent

  data object TouchFrame : HostEvent

  data object TouchCancel : HostEvent

  /** The user asked to close the window. */
  data object Close : HostEvent

  companion object {
    const val PROTOCOL_VERSION = 1

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
      return when (f[0]) {
        "hello" -> need(args, 3) { Hello(int(1), int(2), int(3)) }
        "window" -> need(args, 3) { Window(int(1), int(2), f[3].toDouble()) }
        "pointer_enter" -> need(args, 2) { PointerEnter(float(1), float(2)) }
        "pointer_leave" -> PointerLeave
        "pointer_motion" -> need(args, 3) { PointerMotion(time(1), float(2), float(3)) }
        "pointer_button" -> need(args, 3) { PointerButton(time(1), int(2), bool(3)) }
        "pointer_axis" -> need(args, 3) { PointerAxis(time(1), int(2), f[3].toDouble()) }
        "pointer_axis_value120" -> need(args, 2) { PointerAxisValue120(int(1), int(2)) }
        "pointer_axis_stop" -> need(args, 2) { PointerAxisStop(time(1), int(2)) }
        "pointer_frame" -> PointerFrame
        "keyboard_enter" -> KeyboardEnter
        "keyboard_leave" -> KeyboardLeave
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
        "touch_down" -> need(args, 4) { TouchDown(time(1), int(2), float(3), float(4)) }
        "touch_motion" -> need(args, 4) { TouchMotion(time(1), int(2), float(3), float(4)) }
        "touch_up" -> need(args, 2) { TouchUp(time(1), int(2)) }
        "touch_frame" -> TouchFrame
        "touch_cancel" -> TouchCancel
        "close" -> Close
        else -> null
      }
    }

    // Extra trailing fields are accepted (forward compatibility); missing ones are not.
    private inline fun need(args: Int, n: Int, make: () -> HostEvent): HostEvent? =
      if (args >= n) make() else null
  }
}
