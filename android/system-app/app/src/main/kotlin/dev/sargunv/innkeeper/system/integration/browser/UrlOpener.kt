// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.integration.browser

import android.util.Log
import dev.sargunv.innkeeper.system.bridge.Wire
import dev.sargunv.innkeeper.system.integration.IntegrationFeature

/**
 * Hands http(s) links to the bridge as a HANDOFF URI (host/bridge/src/system/handoff.rs), which
 * opens them in the host browser through the OpenURI portal. The URL is sent as percent-encoded
 * ASCII, and the bridge checks it again.
 */
class UrlOpener(private val feature: IntegrationFeature) {
  /** Returns false if [url] isn't http(s) or the bridge isn't there. */
  fun open(url: String): Boolean {
    val ascii = toAscii(url.trim()) ?: return false
    val lower = ascii.lowercase()
    if (!lower.startsWith("http://") && !lower.startsWith("https://")) return false
    if (ascii.length > MAX) return false
    // HANDOFF metadata: version 1, the URI as text, no subject, no files.
    val metadata = Wire.Writer().u32(1).text(ascii, MAX).text("", 0).bytes()
    Log.i(TAG, "opening on the desktop: ${ascii.take(200)}")
    return feature.connection.send(
      Wire.HANDOFF,
      metadata,
      onReply = { if (!it.ok) Log.w(TAG, "the bridge refused to open the link: ${it.status}") },
    ) {
      // Window 0 means no parent. Action 1 opens the URI, with no files and no prompt.
      u32(0).u8(1).u8(0).u8(0).u8(0).u32(metadata.size)
    }
  }

  companion object {
    const val TAG = "InnkeeperBrowser"
    private const val MAX = 8192
    private const val ALLOWED = "-._~:/?#[]@!$&'()*+,;=%"

    /**
     * Percent-encodes everything outside the URL character set (non-ASCII as UTF-8). Returns null
     * for control characters, which a URL never needs.
     */
    fun toAscii(s: String): String? {
      val o = StringBuilder(s.length)
      for (ch in s) {
        when {
          ch.code < 0x20 || ch.code == 0x7f -> return null
          (ch in 'a'..'z') || (ch in 'A'..'Z') || (ch in '0'..'9') || ALLOWED.indexOf(ch) >= 0 ->
            o.append(ch)
          else ->
            for (b in ch.toString().toByteArray(Charsets.UTF_8)) {
              o.append('%').append(String.format("%02X", b.toInt() and 0xff))
            }
        }
      }
      return o.toString()
    }
  }
}
