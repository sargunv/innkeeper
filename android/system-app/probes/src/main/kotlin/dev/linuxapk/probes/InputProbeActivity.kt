// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.probes

import android.app.Activity
import android.os.Bundle
import android.util.Log
import android.view.KeyEvent
import android.view.MotionEvent
import android.widget.TextView
import android.window.OnBackInvokedDispatcher

/**
 * Diagnostic window that logs every input event it receives (logcat tag `LinuxApkProbe`) and shows
 * the last one. Used by tests/e2e/system-app/run.sh.
 */
class InputProbeActivity : Activity() {
  private lateinit var text: TextView

  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    text =
      TextView(this).apply {
        textSize = 20f
        setPadding(48, 48, 48, 48)
        text = "input probe"
      }
    setContentView(text)
    // Back (predictive back takes KEYCODE_BACK before dispatchKeyEvent) is logged, not acted on,
    // so the window stays for the rest of a test.
    onBackInvokedDispatcher.registerOnBackInvokedCallback(
      OnBackInvokedDispatcher.PRIORITY_DEFAULT,
    ) {
      Log.i(TAG, "back")
      text.text = "back"
    }
  }

  private fun show(what: String, event: Any) {
    val line = "$what $event" + if (event is MotionEvent) gesture(event) else ""
    Log.i(TAG, line)
    text.text = line
  }

  // What MotionEvent.toString leaves out: the classification and the touchpad gesture and
  // stylus axes of each pointer.
  private fun gesture(ev: MotionEvent): String {
    val axes = ArrayList<String>()
    for (i in 0 until ev.pointerCount) {
      for ((name, axis) in AXES) {
        val v = ev.getAxisValue(axis, i)
        if (v != 0f) axes += "$name[$i]=$v"
      }
      if (ev.getPressure(i) != 0f) axes += "pressure[$i]=${ev.getPressure(i)}"
    }
    return " classification=${CLASSIFICATIONS[ev.classification] ?: ev.classification}" +
      " axes={${axes.joinToString(", ")}}"
  }

  override fun dispatchTouchEvent(ev: MotionEvent): Boolean {
    show("touch", ev)
    return super.dispatchTouchEvent(ev)
  }

  override fun dispatchGenericMotionEvent(ev: MotionEvent): Boolean {
    show("generic", ev)
    return super.dispatchGenericMotionEvent(ev)
  }

  override fun dispatchKeyEvent(event: KeyEvent): Boolean {
    show("key", event)
    return super.dispatchKeyEvent(event)
  }

  companion object {
    const val TAG = "LinuxApkProbe"

    private val CLASSIFICATIONS =
      mapOf(
        MotionEvent.CLASSIFICATION_NONE to "NONE",
        MotionEvent.CLASSIFICATION_AMBIGUOUS_GESTURE to "AMBIGUOUS_GESTURE",
        MotionEvent.CLASSIFICATION_DEEP_PRESS to "DEEP_PRESS",
        MotionEvent.CLASSIFICATION_TWO_FINGER_SWIPE to "TWO_FINGER_SWIPE",
        4 to "MULTI_FINGER_SWIPE", // hidden
        MotionEvent.CLASSIFICATION_PINCH to "PINCH",
      )
    private val AXES =
      listOf(
        "scrollX" to MotionEvent.AXIS_GESTURE_SCROLL_X_DISTANCE,
        "scrollY" to MotionEvent.AXIS_GESTURE_SCROLL_Y_DISTANCE,
        "pinch" to MotionEvent.AXIS_GESTURE_PINCH_SCALE_FACTOR,
        "swipeFingers" to 53, // AXIS_GESTURE_SWIPE_FINGER_COUNT, hidden
        "offsetX" to MotionEvent.AXIS_GESTURE_X_OFFSET,
        "tilt" to MotionEvent.AXIS_TILT,
        "orientation" to MotionEvent.AXIS_ORIENTATION,
        "distance" to MotionEvent.AXIS_DISTANCE,
      )
  }
}
