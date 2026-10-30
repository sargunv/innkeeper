// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.probes

import android.app.Activity
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.res.Configuration
import android.os.Bundle
import android.os.PowerManager
import android.util.Log
import android.view.WindowManager
import android.widget.TextView

/** Observable per-window display and keep-awake requests on disposable test desktops. */
class WindowProbe : Activity() {
  private lateinit var text: TextView
  private var wakeLock: PowerManager.WakeLock? = null
  private val identity = System.identityHashCode(this)
  private val control =
    object : BroadcastReceiver() {
      override fun onReceive(context: Context, intent: Intent) {
        when (intent.getStringExtra("mode")) {
          "flag" -> {
            if (intent.getBooleanExtra("enabled", false))
              window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
            else window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
          }
          "lock" -> {
            wakeLock?.let { if (it.isHeld) it.release() }
            wakeLock = null
            if (intent.getBooleanExtra("enabled", false)) {
              wakeLock =
                getSystemService(PowerManager::class.java)
                  .newWakeLock(
                    PowerManager.SCREEN_BRIGHT_WAKE_LOCK,
                    "LinuxApkWindowProbe",
                  )
              wakeLock!!.acquire()
            }
          }
        }
      }
    }

  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    text =
      TextView(this).apply {
        textSize = 24f
        setPadding(24, 24, 24, 24)
      }
    setContentView(text)
    registerReceiver(control, IntentFilter("dev.linuxapk.WINDOW_PROBE"), Context.RECEIVER_EXPORTED)
    report("created")
  }

  override fun onConfigurationChanged(config: Configuration) {
    super.onConfigurationChanged(config)
    report("changed")
  }

  private fun report(event: String) {
    val line =
      "$event instance=$identity display=${display?.displayId} dpi=${resources.configuration.densityDpi} size=${resources.configuration.screenWidthDp}x${resources.configuration.screenHeightDp}"
    text.text = line
    Log.i("LinuxApkWindowProbe", line)
  }

  override fun onDestroy() {
    unregisterReceiver(control)
    wakeLock?.let { if (it.isHeld) it.release() }
    Log.i("LinuxApkWindowProbe", "destroyed instance=$identity")
    super.onDestroy()
  }
}
