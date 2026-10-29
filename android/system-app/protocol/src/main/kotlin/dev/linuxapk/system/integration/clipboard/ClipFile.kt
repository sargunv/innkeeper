// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.integration.clipboard

import dev.linuxapk.system.bridge.Wire
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.charset.CodingErrorAction

/** One file within a kind-3 clipboard selection. */
data class ClipFile(val name: String, val mime: String, val bytes: ByteArray) {
  internal fun encode(): ByteArray {
    validate(name, mime, bytes.size)
    val header = Wire.Writer().text(name, 255).text(mime, 255).u32(bytes.size).bytes()
    return header + bytes
  }

  companion object {
    const val MAX_BYTES = (16 shl 20) - 1024

    /** ClipDescription covers every item, so multiple declared types are ambiguous for a list. */
    fun resolveMime(providerMime: String?, descriptionMimes: List<String>, itemCount: Int): String =
      providerMime
        ?: descriptionMimes.firstOrNull()?.takeIf { itemCount == 1 || descriptionMimes.size == 1 }
        ?: "application/octet-stream"

    fun validate(name: String, mime: String, size: Int) {
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

    internal fun decode(r: ByteBuffer): ClipFile {
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
      require(n <= r.remaining())
      val bytes = ByteArray(n)
      r.get(bytes)
      return ClipFile(name, mime, bytes)
    }
  }
}

/** Kind 3: a file count followed by that many name/MIME/length/bytes records. */
data class ClipFiles(val files: List<ClipFile>) {
  fun encode(): ByteArray {
    require(files.size in 1..MAX_FILES)
    require(encodedSize(files) <= MAX_TOTAL)
    val out = java.io.ByteArrayOutputStream()
    out.write(Wire.Writer().u32(files.size).bytes())
    files.forEach { out.write(it.encode()) }
    return out.toByteArray()
  }

  companion object {
    const val MAX_FILES = 16
    const val MAX_TOTAL = 16 shl 20

    fun isMultiFileSelection(uriSchemes: List<String?>): Boolean =
      uriSchemes.size > 1 && uriSchemes.all { it == "content" }

    fun encodedSize(files: List<ClipFile>): Int =
      4 +
        files.sumOf { 12 + it.name.toByteArray().size + it.mime.toByteArray().size + it.bytes.size }

    fun decode(data: ByteArray): ClipFiles {
      require(data.size in 4..MAX_TOTAL)
      val r = ByteBuffer.wrap(data).order(ByteOrder.LITTLE_ENDIAN)
      val count = r.int
      require(count in 1..MAX_FILES)
      val files = List(count) { ClipFile.decode(r) }
      require(!r.hasRemaining())
      return ClipFiles(files)
    }
  }
}
