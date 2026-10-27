// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.input

/**
 * An Android input event to inject, as plain data. Field values are Android's own constants
 * (`MotionEvent.ACTION_*`, `KeyEvent.KEYCODE_*`, ...); the app turns these into real
 * `MotionEvent`/`KeyEvent` objects. Times are `SystemClock.uptimeMillis()` milliseconds.
 */
sealed interface OutEvent {
  val eventTime: Long

  /** Android display the event targets (`InputEvent.setDisplayId`). */
  val displayId: Int
}

data class OutPointer(
  val id: Int,
  val toolType: Int,
  val x: Float,
  val y: Float,
  val pressure: Float,
  val vscroll: Float = 0f,
  val hscroll: Float = 0f,
)

data class OutMotion(
  val source: Int,
  /** Full action, including the pointer index for POINTER_DOWN/UP. */
  val action: Int,
  val actionButton: Int,
  val buttonState: Int,
  val metaState: Int,
  val downTime: Long,
  override val eventTime: Long,
  val pointers: List<OutPointer>,
  override val displayId: Int = 0,
) : OutEvent

data class OutKey(
  val action: Int,
  val keyCode: Int,
  val scanCode: Int,
  val metaState: Int,
  val flags: Int,
  val downTime: Long,
  override val eventTime: Long,
  override val displayId: Int = 0,
) : OutEvent
