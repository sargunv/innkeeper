// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.probes

import android.app.Activity
import android.os.Bundle
import android.util.Log
import android.view.InputDevice
import android.view.KeyEvent
import android.view.MotionEvent
import android.widget.TextView

/**
 * Click to capture, R to release, and C to request again. Escape deliberately keeps the capture.
 */
class CaptureProbeActivity : Activity() {
  private lateinit var probe: TextView
  private var count = 0
  private var totalX = 0f
  private var totalY = 0f

  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    probe =
      object : TextView(this) {
          override fun onPointerCaptureChange(hasCapture: Boolean) {
            super.onPointerCaptureChange(hasCapture)
            Log.i(TAG, "capture=$hasCapture display=${display?.displayId}")
            report()
          }

          override fun onCapturedPointerEvent(event: MotionEvent): Boolean {
            if (event.actionMasked == MotionEvent.ACTION_MOVE) {
              count++
              totalX += event.x
              totalY += event.y
            }
            Log.i(
              TAG,
              "relative source=${event.source} x=${event.x} y=${event.y} " +
                "device=${event.device?.name} count=$count",
            )
            check(event.isFromSource(InputDevice.SOURCE_MOUSE_RELATIVE))
            report()
            return true
          }
        }
        .apply {
          textSize = 24f
          setPadding(48, 48, 48, 48)
          isFocusableInTouchMode = true
          setOnClickListener { requestPointerCapture() }
        }
    setContentView(probe)
    probe.requestFocus()
    report()
  }

  private fun report() {
    probe.text =
      "Pointer capture probe\n\n" +
        "Capture: ${probe.hasPointerCapture()}\n" +
        "Relative reports: $count\nTotal: x=$totalX y=$totalY\n\n" +
        "Click or C: capture\nR: release\nEscape: host unlock (probe retains capture)"
  }

  override fun dispatchKeyEvent(event: KeyEvent): Boolean {
    if (event.action == KeyEvent.ACTION_DOWN) {
      when (event.keyCode) {
        KeyEvent.KEYCODE_C -> probe.requestPointerCapture()
        KeyEvent.KEYCODE_R -> probe.releasePointerCapture()
        KeyEvent.KEYCODE_ESCAPE -> return true
        else -> return super.dispatchKeyEvent(event)
      }
      return true
    }
    return super.dispatchKeyEvent(event)
  }

  companion object {
    const val TAG = "InnkeeperCaptureProbe"
  }
}
