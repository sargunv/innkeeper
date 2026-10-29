// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.probes

import android.app.Activity
import android.content.Intent
import android.content.pm.ActivityInfo
import android.os.Bundle
import android.util.Log
import android.view.WindowInsets
import android.widget.Button
import android.widget.LinearLayout
import android.widget.TextView

/** Public API requests for the windowing e2e group and nested-desktop proof. */
class WindowModeProbe : Activity() {
  private lateinit var status: TextView

  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    status = TextView(this).apply { textSize = 24f }
    setContentView(
      LinearLayout(this).apply {
        orientation = LinearLayout.VERTICAL
        setPadding(24, 24, 24, 24)
        addView(status)
        for (action in listOf("portrait", "landscape", "immersive", "restore", "fullscreen-api")) {
          addView(
            Button(this@WindowModeProbe).apply {
              text = action
              setOnClickListener { applyAction(action) }
            },
          )
        }
      },
    )
    applyAction(intent.getStringExtra("action") ?: "portrait")
  }

  override fun onNewIntent(intent: Intent) {
    super.onNewIntent(intent)
    setIntent(intent)
    applyAction(intent.getStringExtra("action") ?: "portrait")
  }

  private fun applyAction(action: String) {
    when (action) {
      "portrait" -> requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_PORTRAIT
      "landscape" -> requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_LANDSCAPE
      "immersive" -> window.insetsController?.hide(WindowInsets.Type.systemBars())
      "restore" -> window.insetsController?.show(WindowInsets.Type.systemBars())
      "fullscreen-api" -> requestFullscreenMode(FULLSCREEN_MODE_REQUEST_ENTER, null)
    }
    status.text = "Linux.apk window probe\n$action\nCtrl+R: rotate / F11: fullscreen"
    Log.i("LinuxApkWindowModeProbe", "action=$action display=${display?.displayId}")
  }
}
