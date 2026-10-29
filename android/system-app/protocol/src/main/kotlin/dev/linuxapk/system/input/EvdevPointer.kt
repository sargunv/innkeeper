// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.input

import android.view.InputDevice
import android.view.MotionEvent
import kotlin.math.roundToInt

/** Predict InputReader's clipped cursor position, including a drag outside the host surface. */
class EvdevPointer {
  data class Step(val dx: Int, val dy: Int, val x: Float, val y: Float)

  private var width = 0
  private var height = 0
  private var last: Step? = null

  fun reset() {
    last = null
  }

  fun move(x: Float, y: Float, initialX: Float, initialY: Float, w: Int, h: Int): Step {
    if (w != width || h != height) reset()
    width = w
    height = h
    val cx = (last?.x ?: initialX).coerceIn(0f, (w - 1).coerceAtLeast(0).toFloat())
    val cy = (last?.y ?: initialY).coerceIn(0f, (h - 1).coerceAtLeast(0).toFloat())
    val dx = (x - cx).roundToInt()
    val dy = (y - cy).roundToInt()
    return Step(
        dx,
        dy,
        (cx + dx).coerceIn(0f, (w - 1).coerceAtLeast(0).toFloat()),
        (cy + dy).coerceIn(0f, (h - 1).coerceAtLeast(0).toFloat()),
      )
      .also { last = it }
  }

  companion object {
    /** Physical mouse events have one route, regardless of modifiers or gesture phase. */
    fun accepts(event: OutMotion): Boolean =
      event.source == InputDevice.SOURCE_MOUSE &&
        event.classification == 0 &&
        event.pointers.singleOrNull()?.toolType == MotionEvent.TOOL_TYPE_MOUSE
  }
}
