// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.probes

import android.app.Activity
import android.content.Intent
import android.os.Bundle
import android.util.Log
import android.widget.Button

/** A second task deliberately starts on the caller's display, as an app's New Window does. */
class TaskWindowProbe : Activity() {
  override fun onCreate(state: Bundle?) {
    super.onCreate(state)
    setContentView(
      Button(this).apply {
        text = "New window (task $taskId)"
        setOnClickListener { newWindow() }
      },
    )
  }

  override fun onNewIntent(intent: Intent) {
    super.onNewIntent(intent)
    if (intent.getBooleanExtra("new-window", false)) newWindow()
  }

  private fun newWindow() {
    Log.i(
      "InnkeeperTaskWindow",
      "new task requested from task=$taskId display=${display?.displayId}",
    )
    startActivity(
      Intent(this, TaskWindowProbe::class.java)
        .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_MULTIPLE_TASK),
    )
  }
}
