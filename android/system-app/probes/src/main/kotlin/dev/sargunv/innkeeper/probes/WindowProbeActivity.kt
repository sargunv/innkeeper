// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.probes

import android.app.Activity
import android.graphics.Color
import android.graphics.drawable.GradientDrawable
import android.os.Bundle
import android.util.Log
import android.util.TypedValue
import android.view.Gravity
import android.view.ViewGroup.LayoutParams.MATCH_PARENT
import android.view.ViewGroup.LayoutParams.WRAP_CONTENT
import android.view.WindowInsets
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.TextView

/**
 * Diagnostic window for docs/display.md. It shows and logs (tag `InnkeeperProbe`) the window's
 * system-bar and IME insets and its size, over a card with 24 dp elevation on a light background,
 * so a screenshot shows whether elevation shadows render.
 *
 * `innkeeper run dev.sargunv.innkeeper.probes` opens the probes' launcher. The e2e tests launch
 * this activity through the session API's `Launch` with its component.
 */
class WindowProbeActivity : Activity() {
  private lateinit var text: TextView

  private fun dp(v: Float) =
    TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_DIP, v, resources.displayMetrics)

  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    text =
      TextView(this).apply {
        textSize = 18f
        setTextColor(Color.BLACK)
        setPadding(dp(24f).toInt(), dp(24f).toInt(), dp(24f).toInt(), dp(24f).toInt())
      }
    val card =
      FrameLayout(this).apply {
        background =
          GradientDrawable().apply {
            setColor(Color.WHITE)
            cornerRadius = dp(16f)
          }
        elevation = dp(24f)
        addView(text)
      }
    val root =
      LinearLayout(this).apply {
        setBackgroundColor(Color.rgb(0xEE, 0xEE, 0xEE))
        gravity = Gravity.CENTER
        addView(
          card,
          LinearLayout.LayoutParams(WRAP_CONTENT, WRAP_CONTENT).apply {
            val m = dp(48f).toInt()
            setMargins(m, m, m, m)
          },
        )
      }
    root.setOnApplyWindowInsetsListener { v, insets ->
      report(insets, v.width, v.height)
      insets
    }
    root.addOnLayoutChangeListener { v, _, _, _, _, _, _, _, _ ->
      v.rootWindowInsets?.let { report(it, v.width, v.height) }
    }
    setContentView(root, android.view.ViewGroup.LayoutParams(MATCH_PARENT, MATCH_PARENT))
  }

  private var last = ""

  private fun report(insets: WindowInsets, w: Int, h: Int) {
    val bars = insets.getInsets(WindowInsets.Type.systemBars())
    val status = insets.getInsets(WindowInsets.Type.statusBars())
    val nav = insets.getInsets(WindowInsets.Type.navigationBars())
    val ime = insets.getInsets(WindowInsets.Type.ime())
    val line =
      "display=${display?.displayId} size=${w}x$h density=${resources.displayMetrics.densityDpi} " +
        "statusBars=$status navigationBars=$nav systemBars=$bars ime=$ime"
    if (line == last) return
    last = line
    Log.i(TAG, line)
    text.text = "Elevation 24dp card\n\n" + line.replace(" ", "\n")
  }

  companion object {
    const val TAG = "InnkeeperProbe"
  }
}
