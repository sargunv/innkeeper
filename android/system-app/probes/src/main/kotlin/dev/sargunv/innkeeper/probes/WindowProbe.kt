// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.probes

import android.app.Activity
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.res.Configuration
import android.graphics.Color
import android.os.Bundle
import android.os.PowerManager
import android.util.Log
import android.view.WindowInsets
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
                    "InnkeeperWindowProbe",
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
        setTextColor(Color.BLACK)
        setOnApplyWindowInsetsListener { view, insets ->
          val bars = insets.getInsets(WindowInsets.Type.systemBars())
          val margin = (24 * resources.displayMetrics.density).toInt()
          view.setPadding(
            bars.left + margin,
            bars.top + margin,
            bars.right + margin,
            bars.bottom + margin,
          )
          insets
        }
      }
    setContentView(text)
    registerReceiver(
      control,
      IntentFilter("dev.sargunv.innkeeper.WINDOW_PROBE"),
      Context.RECEIVER_EXPORTED,
    )
    report("created")
  }

  override fun onConfigurationChanged(config: Configuration) {
    super.onConfigurationChanged(config)
    report("changed")
  }

  private fun report(event: String) {
    val line =
      "$event instance=$identity display=${display?.displayId} dpi=${resources.configuration.densityDpi} size=${resources.configuration.screenWidthDp}x${resources.configuration.screenHeightDp}"
    text.textSize = 24f
    text.text = line.replace(" ", "\n")
    Log.i("InnkeeperWindowProbe", line)
  }

  override fun onDestroy() {
    unregisterReceiver(control)
    wakeLock?.let { if (it.isHeld) it.release() }
    Log.i("InnkeeperWindowProbe", "destroyed instance=$identity")
    super.onDestroy()
  }
}
