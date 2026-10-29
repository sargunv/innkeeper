// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.integration.clipboard

import dev.linuxapk.system.bridge.Wire
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.charset.CodingErrorAction

/** Kind 3 of CLIP/CLIP_SET: name, MIME, byte count, bytes; total <= 16 MiB. */
data class ClipFile(val name: String, val mime: String, val bytes: ByteArray) {
  fun encode(): ByteArray {
    validate(name, mime, bytes.size)
    val header = Wire.Writer().text(name, 255).text(mime, 255).u32(bytes.size).bytes()
    return header + bytes
  }

  companion object {
    const val MAX_BYTES = (16 shl 20) - 1024

    private fun validate(name: String, mime: String, size: Int) {
      require(
        name.isNotEmpty() &&
          name != "." &&
          name != ".." &&
          !name.contains('/') &&
          !name.contains('\\'),
      )
      require(name.toByteArray().size <= 255 && name.none { it.isISOControl() })
      require(
        mime.contains('/') &&
          mime.toByteArray().size <= 255 &&
          mime.none { it.isWhitespace() || it.isISOControl() },
      )
      require(size in 0..MAX_BYTES)
    }

    fun decode(data: ByteArray): ClipFile {
      require(data.size <= 16 shl 20)
      val r = ByteBuffer.wrap(data).order(ByteOrder.LITTLE_ENDIAN)
      fun text(): String {
        require(r.remaining() >= 4)
        val n = r.int
        require(n in 0..255 && n <= r.remaining())
        val b = ByteArray(n)
        r.get(b)
        return Charsets.UTF_8.newDecoder()
          .onMalformedInput(CodingErrorAction.REPORT)
          .decode(ByteBuffer.wrap(b))
          .toString()
      }
      val name = text()
      val mime = text()
      require(r.remaining() >= 4)
      val n = r.int
      validate(name, mime, n)
      require(n == r.remaining())
      val bytes = ByteArray(n)
      r.get(bytes)
      return ClipFile(name, mime, bytes)
    }
  }
}
