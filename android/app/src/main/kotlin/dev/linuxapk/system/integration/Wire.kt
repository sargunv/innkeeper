// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.integration

import android.graphics.Bitmap
import android.system.Os
import android.system.OsConstants
import java.io.ByteArrayOutputStream
import java.io.FileDescriptor
import java.nio.ByteBuffer
import java.nio.ByteOrder

/**
 * The bridge protocol (docs/bridge.md): little-endian messages of at most 512 bytes over
 * SOCK_SEQPACKET, and the system service's types. Large payloads go in a sealed memfd next to a
 * small request ([Blob]).
 */
object Wire {
  const val MAX_MSG = 512
  const val HDR = 8
  const val REPLY = 0x8000
  const val HELLO = 0x0001
  const val SERVICE_SYSTEM = 3

  // Requests.
  const val START = 0x0301
  const val NOTIFY = 0x0302
  const val NOTIFY_CLOSE = 0x0303
  const val CLIP_SET = 0x0304
  const val OPEN_URI = 0x0305
  const val MEDIA = 0x0306
  const val MEDIA_CLOSE = 0x0307

  // Events.
  const val EV_SETTINGS = 0x4301
  const val EV_NOTIFY = 0x4302
  const val EV_CLIP = 0x4303
  const val EV_MEDIA_CMD = 0x4304

  // START features.
  const val F_SETTINGS = 1
  const val F_CLIPBOARD = 2
  const val F_NOTIFICATIONS = 4
  const val F_MEDIA = 8
  const val F_OPEN_URI = 16

  const val CLIP_TEXT = 1
  const val CLIP_IMAGE = 2

  /** A little-endian message (or blob) builder. */
  class Writer(capacity: Int = 64) {
    private val out = ByteArrayOutputStream(capacity)

    fun u8(v: Int) = apply { out.write(v and 0xff) }

    fun u16(v: Int) = apply {
      out.write(v and 0xff)
      out.write((v ushr 8) and 0xff)
    }

    fun u32(v: Int) = apply { u16(v and 0xffff).u16(v ushr 16) }

    fun i32(v: Int) = u32(v)

    fun u64(v: Long) = apply { u32(v.toInt()).u32((v ushr 32).toInt()) }

    fun f32(v: Float) = u32(java.lang.Float.floatToRawIntBits(v))

    /** A message name: u16 length, at most 96 bytes of UTF-8. */
    fun name(s: String) = apply {
      val b = truncateUtf8(s, 96)
      u16(b.size)
      out.write(b)
    }

    /** A blob text field: u32 length, then UTF-8 (at most [max] bytes). */
    fun text(s: String?, max: Int) = apply {
      val b = truncateUtf8(s ?: "", max)
      u32(b.size)
      out.write(b)
    }

    /** A blob image: u16 width, u16 height, RGBA bytes (0x0: none). */
    fun image(img: Rgba?) = apply {
      if (img == null) {
        u16(0).u16(0)
      } else {
        u16(img.width).u16(img.height)
        out.write(img.bytes)
      }
    }

    fun bytes(): ByteArray = out.toByteArray()

    val size: Int
      get() = out.size()
  }

  fun message(type: Int, tag: Int): Writer = Writer().u16(type).u16(0).u32(tag)

  fun reader(b: ByteArray, off: Int, len: Int): ByteBuffer =
    ByteBuffer.wrap(b, off, len).slice().order(ByteOrder.LITTLE_ENDIAN)

  /** Reads a message name (u16 length + UTF-8). */
  fun name(r: ByteBuffer): String {
    val n = r.short.toInt() and 0xffff
    require(n <= 96 && n <= r.remaining()) { "bad name" }
    val b = ByteArray(n)
    r.get(b)
    return String(b, Charsets.UTF_8)
  }

  /** At most [max] bytes of [s]'s UTF-8, cut on a character boundary. */
  fun truncateUtf8(s: String, max: Int): ByteArray {
    val b = s.toByteArray(Charsets.UTF_8)
    if (b.size <= max) return b
    var n = max
    while (n > 0 && (b[n].toInt() and 0xc0) == 0x80) n--
    return b.copyOf(n)
  }

  /** An RGBA image (not premultiplied), as the bridge takes it. */
  class Rgba(val width: Int, val height: Int, val bytes: ByteArray)

  /** Scales [bmp] to fit [maxSide] and converts it to [Rgba]. */
  fun rgba(bmp: Bitmap, maxSide: Int): Rgba? {
    if (bmp.width <= 0 || bmp.height <= 0) return null
    val scale = minOf(1f, maxSide.toFloat() / maxOf(bmp.width, bmp.height))
    val w = maxOf(1, (bmp.width * scale).toInt())
    val h = maxOf(1, (bmp.height * scale).toInt())
    val src =
      if (w != bmp.width || h != bmp.height) Bitmap.createScaledBitmap(bmp, w, h, true)
      else if (bmp.config == Bitmap.Config.HARDWARE) bmp.copy(Bitmap.Config.ARGB_8888, false)
      else bmp
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
    return Rgba(w, h, out)
  }
}

/** Sealed memfds: how payloads too big for a message cross the socket. */
object Blob {
  private const val MFD_ALLOW_SEALING = 2
  private const val F_ADD_SEALS = 1033
  private const val F_SEAL_SEAL = 1
  private const val F_SEAL_SHRINK = 2
  private const val F_SEAL_GROW = 4
  private const val F_SEAL_WRITE = 8

  /** A memfd holding [bytes], sealed against writes and resizing. The caller closes it. */
  fun create(bytes: ByteArray): FileDescriptor {
    val fd = Os.memfd_create("linuxapk-blob", OsConstants.MFD_CLOEXEC or MFD_ALLOW_SEALING)
    try {
      var off = 0
      while (off < bytes.size) off += Os.write(fd, bytes, off, bytes.size - off)
      Os.fcntlInt(fd, F_ADD_SEALS, F_SEAL_SHRINK or F_SEAL_GROW or F_SEAL_WRITE or F_SEAL_SEAL)
    } catch (e: Exception) {
      Os.close(fd)
      throw e
    }
    return fd
  }

  /** Reads [len] bytes (at most [max]) from a memfd the bridge sent. */
  fun read(fd: FileDescriptor, len: Int, max: Int): ByteArray {
    require(len in 0..max) { "blob too long" }
    require(Os.fstat(fd).st_size >= len) { "blob shorter than declared" }
    val b = ByteArray(len)
    var off = 0
    while (off < len) {
      val n = Os.pread(fd, b, off, len - off, off.toLong())
      require(n > 0) { "short blob" }
      off += n
    }
    return b
  }

  fun close(fd: FileDescriptor?) {
    try {
      if (fd != null && fd.valid()) Os.close(fd)
    } catch (_: Exception) {}
  }
}
