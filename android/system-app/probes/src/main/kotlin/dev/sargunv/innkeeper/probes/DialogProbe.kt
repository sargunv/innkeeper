// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.probes

import android.app.Activity
import android.os.Bundle
import android.util.Log
import android.widget.Button

/**
 * A dialog-themed activity, like a share target, and a translucent one that fills its display with
 * a small view, like a floating button. A touch outside doesn't finish the dialog, so only a click
 * that lands on its button counts.
 */
open class DialogProbe : Activity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    setFinishOnTouchOutside(false)
    setContentView(
      Button(this).apply {
        text = "Dialog probe"
        textSize = 24f
        setOnClickListener { Log.i(TAG, "clicked") }
      },
    )
    Log.i(TAG, "${javaClass.simpleName} created display=${display?.displayId}")
  }

  class Translucent : DialogProbe()

  companion object {
    private const val TAG = "InnkeeperDialogProbe"
  }
}
