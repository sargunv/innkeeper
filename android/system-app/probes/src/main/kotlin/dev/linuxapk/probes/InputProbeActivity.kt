// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.probes

import android.app.Activity
import android.os.Bundle
import android.util.Log
import android.view.KeyEvent
import android.view.MotionEvent
import android.widget.TextView

/**
 * Diagnostic window that logs every input event it receives (logcat tag `LinuxApkProbe`) and shows
 * the last one. Used by the end-to-end test in docs/input.md.
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
  }

  private fun show(what: String, event: Any) {
    val line = "$what $event"
    Log.i(TAG, line)
    text.text = line
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
  }
}
