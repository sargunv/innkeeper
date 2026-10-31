// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.probes

import android.app.Activity
import android.app.ActivityManager.TaskDescription
import android.content.Intent
import android.graphics.Color
import android.os.Bundle
import android.util.Log
import android.widget.Button

/** Visible two-screen navigation and launcher-root Back for nested desktop proof. */
class BackProbe : Activity() {
  @Suppress("DEPRECATION")
  override fun onCreate(state: Bundle?) {
    super.onCreate(state)
    setTaskDescription(TaskDescription("Back probe", null, Color.rgb(25, 118, 210)))
    setContentView(
      Button(this).apply {
        text = "Root screen — open details"
        setOnClickListener { startActivity(Intent(this@BackProbe, Details::class.java)) }
      },
    )
  }

  override fun onResume() {
    super.onResume()
    Log.i("InnkeeperBackProbe", "root resumed task=$taskId display=${display?.displayId}")
  }

  class Details : Activity() {
    override fun onCreate(state: Bundle?) {
      super.onCreate(state)
      setContentView(Button(this).apply { text = "Details — use title-bar Back" })
      Log.i("InnkeeperBackProbe", "details task=$taskId display=${display?.displayId}")
    }
  }
}
