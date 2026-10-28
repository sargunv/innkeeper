// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.probes

import android.app.Activity
import android.os.Bundle
import android.text.Editable
import android.text.TextWatcher
import android.util.Log
import android.view.KeyEvent
import android.widget.EditText

/**
 * A focused text field for the keyboard checks (docs/input.md): logs each key it receives and the
 * field's whole text after every change (logcat tag `LinuxApkProbe`, `text [<text>]`).
 */
class TextProbeActivity : Activity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    val field =
      EditText(this).apply {
        textSize = 24f
        hint = "text probe"
        addTextChangedListener(
          object : TextWatcher {
            override fun beforeTextChanged(s: CharSequence, a: Int, b: Int, c: Int) {}

            override fun onTextChanged(s: CharSequence, a: Int, b: Int, c: Int) {}

            override fun afterTextChanged(s: Editable) {
              Log.i(TAG, "text [$s]")
            }
          },
        )
      }
    setContentView(field)
    field.requestFocus()
  }

  override fun dispatchKeyEvent(event: KeyEvent): Boolean {
    Log.i(TAG, "key $event")
    return super.dispatchKeyEvent(event)
  }

  companion object {
    const val TAG = InputProbeActivity.TAG
  }
}
