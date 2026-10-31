// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.probes

import android.app.Activity
import android.content.pm.ActivityInfo
import android.graphics.Color
import android.os.Bundle
import android.view.Gravity
import android.widget.TextView

/** Requests wide colour; the baseline desktop may display its SDR equivalent. */
open class WideColorProbe : Activity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    window.colorMode =
      if (this is Hdr) ActivityInfo.COLOR_MODE_HDR else ActivityInfo.COLOR_MODE_WIDE_COLOR_GAMUT
    setContentView(
      TextView(this).apply {
        text = "Wide colour / HDR probe\nSDR equivalent on baseline desktops"
        gravity = Gravity.CENTER
        textSize = 24f
        setTextColor(Color.WHITE)
        setBackgroundColor(Color.rgb(240, 80, 32))
      },
    )
  }

  class Hdr : WideColorProbe()
}
