// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.input

import android.view.MotionEvent
import kotlin.math.roundToInt

/** Predict InputReader's clipped cursor position, including a drag outside the host surface. */
class EvdevPointer {
  data class Step(val dx: Int, val dy: Int, val x: Float, val y: Float)

  private var width = 0
  private var height = 0
  private var last: Step? = null
  private var dragRoute: Boolean? = null
  private var injectedHover: OutMotion? = null

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

  /** Remember the independently tracked device -1 hover while modifiers require injection. */
  fun injected(event: OutMotion) {
    when (event.action and MotionEvent.ACTION_MASK) {
      MotionEvent.ACTION_HOVER_ENTER,
      MotionEvent.ACTION_HOVER_MOVE -> injectedHover = event
      MotionEvent.ACTION_HOVER_EXIT,
      MotionEvent.ACTION_DOWN -> injectedHover = null
    }
  }

  /** End device -1's hover before resuming native input, including an unmodified leave. */
  fun exitInjected(time: Long): OutMotion? {
    val hover = injectedHover ?: return null
    injectedHover = null
    return hover.copy(action = MotionEvent.ACTION_HOVER_EXIT, eventTime = time)
  }

  /** A modifier fallback applies to the entire button sequence, including its release. */
  fun native(action: Int, buttons: Int, meta: Int, nativeMeta: Int): Boolean {
    if (buttons != 0 && dragRoute == null) dragRoute = meta == nativeMeta
    val route = dragRoute ?: (meta == nativeMeta)
    if (action == MotionEvent.ACTION_UP || action == MotionEvent.ACTION_CANCEL) dragRoute = null
    if (!route) reset() // injected motion may change Android's actual cursor position
    return route
  }
}
