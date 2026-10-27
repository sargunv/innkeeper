// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.input

import java.io.ByteArrayOutputStream

/**
 * Text in protocol lines (the bridge's `display/ime.rs`): bytes up to space, DEL and `%` are `%XX`;
 * everything else, including non-ASCII UTF-8, is sent as is. The bridge refuses any other form, so
 * [encode] must match it exactly.
 */
object PercentText {
  private fun escaped(b: Int) = b <= 0x20 || b == 0x7f || b == '%'.code

  fun encode(s: String): String {
    val out = StringBuilder(s.length)
    for (ch in s) {
      if (ch.code < 0x80 && escaped(ch.code)) out.append("%%%02X".format(ch.code))
      else out.append(ch)
    }
    return out.toString()
  }

  /** Decodes, or null for a malformed escape or invalid UTF-8. */
  fun decode(s: String): String? {
    val b = s.toByteArray(Charsets.UTF_8)
    val out = ByteArrayOutputStream(b.size)
    var i = 0
    while (i < b.size) {
      val c = b[i].toInt() and 0xff
      if (c == '%'.code) {
        if (i + 2 >= b.size) return null
        val v = String(b, i + 1, 2, Charsets.US_ASCII).toIntOrNull(16) ?: return null
        out.write(v)
        i += 3
      } else {
        out.write(c)
        i++
      }
    }
    val bytes = out.toByteArray()
    val decoder = Charsets.UTF_8.newDecoder()
    return try {
      decoder.decode(java.nio.ByteBuffer.wrap(bytes)).toString()
    } catch (_: java.nio.charset.CharacterCodingException) {
      null
    }
  }
}
