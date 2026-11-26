// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.probes

import android.app.Activity
import android.app.AlertDialog
import android.content.Intent
import android.os.Bundle
import android.util.Log
import android.widget.Button

/**
 * A dialog-themed activity, like a share target, and a translucent one whose UI fills its display,
 * like a floating button or an app that swipes back to the activity behind it. A touch outside
 * doesn't finish the dialog, so only a click that lands on its button counts.
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

  /** `--ez popup true` opens an app dialog over the activity's UI. */
  class Translucent : DialogProbe() {
    override fun onNewIntent(intent: Intent) {
      super.onNewIntent(intent)
      if (!intent.getBooleanExtra("popup", false)) return
      AlertDialog.Builder(this)
        .setMessage("Dialog probe popup")
        .setPositiveButton("OK", null)
        .show()
      Log.i(TAG, "popup shown")
    }
  }

  companion object {
    private const val TAG = "InnkeeperDialogProbe"
  }
}
