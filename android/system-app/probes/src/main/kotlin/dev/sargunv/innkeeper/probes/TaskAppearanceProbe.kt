// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.probes

import android.app.Activity
import android.app.ActivityManager.TaskDescription
import android.content.Intent
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.os.Bundle
import android.util.Log
import android.widget.Button
import android.widget.LinearLayout
import android.widget.TextView

/** Changes the same task's description for nested KWin and Mutter visual proof. */
class TaskAppearanceProbe : Activity() {
  private lateinit var status: TextView

  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    status = TextView(this).apply { textSize = 26f }
    setContentView(
      LinearLayout(this).apply {
        orientation = LinearLayout.VERTICAL
        setPadding(32, 32, 32, 32)
        addView(status)
        for (state in listOf("blue", "orange", "clear")) {
          addView(
            Button(this@TaskAppearanceProbe).apply {
              text = state
              setOnClickListener { change(state) }
            },
          )
        }
      },
    )
    change(intent.getStringExtra("state") ?: "blue")
  }

  override fun onNewIntent(intent: Intent) {
    super.onNewIntent(intent)
    setIntent(intent)
    change(intent.getStringExtra("state") ?: "blue")
  }

  @Suppress("DEPRECATION")
  private fun change(state: String) {
    val color =
      when (state) {
        "orange" -> Color.rgb(239, 108, 0)
        "clear" -> 0
        else -> Color.rgb(25, 118, 210)
      }
    val icon =
      if (color == 0) null
      else
        Bitmap.createBitmap(512, 512, Bitmap.Config.ARGB_8888).apply {
          val canvas = Canvas(this)
          canvas.drawColor(color)
          val paint = Paint(Paint.ANTI_ALIAS_FLAG).apply { this.color = Color.WHITE }
          if (state == "orange") canvas.drawRect(120f, 120f, 392f, 392f, paint)
          else canvas.drawCircle(256f, 256f, 150f, paint)
        }
    setTaskDescription(TaskDescription("Task appearance $state", icon, color))
    status.text =
      "TaskDescription: $state\nPrimary: #%08X\nIcon: %s\nTask: $taskId"
        .format(
          color,
          if (state == "orange") "white square"
          else if (state == "clear") "default" else "white circle",
        )
    Log.i("InnkeeperTaskAppearance", "state=$state task=$taskId display=${display?.displayId}")
  }
}
