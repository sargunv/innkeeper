// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.probes

import android.app.Activity
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.os.Bundle
import android.util.Log
import android.view.PointerIcon
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.TextView

/** Three visible regions for checking the host cursor and the absence of a window cursor. */
class CursorProbeActivity : Activity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    val root =
      LinearLayout(this).apply {
        orientation = LinearLayout.VERTICAL
        setPadding(48, 48, 48, 48)
      }
    val bitmap = Bitmap.createBitmap(32, 32, Bitmap.Config.ARGB_8888)
    val canvas = Canvas(bitmap)
    val paint =
      Paint().apply {
        color = Color.MAGENTA
        strokeWidth = 3f
      }
    canvas.drawLine(0f, 11f, 31f, 11f, paint)
    canvas.drawLine(7f, 0f, 7f, 31f, paint)
    for ((name, view) in
      listOf(
        "text" to
          EditText(this).apply {
            setText("I-beam over this text field")
            textSize = 24f
            setSingleLine()
          },
        "hidden" to
          TextView(this).apply {
            text = "TYPE_NULL: cursor hidden here"
            textSize = 24f
            pointerIcon = PointerIcon.getSystemIcon(context, PointerIcon.TYPE_NULL)
          },
        "custom" to
          TextView(this).apply {
            text = "Custom bitmap: magenta cross, hotspot (7, 11)"
            textSize = 24f
            pointerIcon = PointerIcon.create(bitmap, 7f, 11f)
          },
      )) {
      view.setOnHoverListener { _, event ->
        Log.i(
          "InnkeeperCursorProbe",
          "$name action=${event.actionMasked} x=${event.x} y=${event.y} device=${event.deviceId}",
        )
        false
      }
      root.addView(view, LinearLayout.LayoutParams(-1, 0, 1f))
    }
    setContentView(root)
  }
}
