// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.bridge

import android.graphics.Bitmap

/** This bitmap scaled to fit [maxSide], as the bridge's RGBA (null if it's empty). */
fun Bitmap.toRgba(maxSide: Int): Wire.Rgba? {
  if (width <= 0 || height <= 0) return null
  val scale = minOf(1f, maxSide.toFloat() / maxOf(width, height))
  val w = maxOf(1, (width * scale).toInt())
  val h = maxOf(1, (height * scale).toInt())
  val src =
    if (w != width || h != height) Bitmap.createScaledBitmap(this, w, h, true)
    else if (config == Bitmap.Config.HARDWARE) copy(Bitmap.Config.ARGB_8888, false) else this
  val px = IntArray(w * h)
  src.getPixels(px, 0, w, 0, 0, w, h) // unpremultiplied ARGB
  val out = ByteArray(w * h * 4)
  for (i in px.indices) {
    val c = px[i]
    out[i * 4] = (c ushr 16).toByte()
    out[i * 4 + 1] = (c ushr 8).toByte()
    out[i * 4 + 2] = c.toByte()
    out[i * 4 + 3] = (c ushr 24).toByte()
  }
  return Wire.Rgba(w, h, out)
}
