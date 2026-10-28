// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.bridge

import java.io.ByteArrayOutputStream
import java.nio.ByteBuffer
import java.nio.ByteOrder

/**
 * The bridge protocol (docs/bridge.md, host/bridge/src/ipc/wire.rs): little-endian messages of at
 * most 512 bytes over SOCK_SEQPACKET, and the system service's types. Large payloads go in a sealed
 * memfd next to a small request. `host/bridge/src/ipc/wire-fixtures.txt` holds encodings both sides
 * are tested against.
 */
object Wire {
  const val MAX_MSG = 512
  const val HDR = 8
  const val MAX_NAME = 96
  const val MAJOR = 1
  const val MINOR = 0
  const val REPLY = 0x8000
  const val HELLO = 0x0001
  const val EVENT_MIN = 0x4000
  const val EVENT_MAX = 0x7fff

  // System service requests.
  const val START = 0x0301
  const val NOTIFY = 0x0302
  const val NOTIFY_CLOSE = 0x0303
  const val CLIP_SET = 0x0304
  const val OPEN_URI = 0x0305
  const val MEDIA = 0x0306
  const val MEDIA_CLOSE = 0x0307

  // System service events.
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

    /** A message name: u16 length, at most [MAX_NAME] bytes of UTF-8. */
    fun name(s: String) = line(s, MAX_NAME)

    /** A line: u16 length, then UTF-8 (at most [max] bytes). */
    fun line(s: String, max: Int) = apply {
      val b = truncateUtf8(s, max)
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

  /** The first message on every connection: binds it to [service]. */
  fun hello(tag: Int, service: Int, name: String): ByteArray =
    message(HELLO, tag).u16(MAJOR).u16(MINOR).u32(service).name(name).bytes()

  /** A received message: its header and a little-endian reader over its body. */
  data class Message(val type: Int, val tag: Int, val body: ByteBuffer)

  /** Parses the first [len] bytes of [buf]; null if that isn't a message's size. */
  fun parse(buf: ByteArray, len: Int): Message? {
    if (len < HDR || len > MAX_MSG) return null
    val r = ByteBuffer.wrap(buf, 0, len).order(ByteOrder.LITTLE_ENDIAN)
    val type = r.short.toInt() and 0xffff
    r.short // flags
    val tag = r.int
    val body = ByteBuffer.wrap(buf.copyOfRange(HDR, len)).order(ByteOrder.LITTLE_ENDIAN)
    return Message(type, tag, body)
  }

  /** Reads a message name (u16 length + UTF-8). */
  fun name(r: ByteBuffer): String {
    val n = r.short.toInt() and 0xffff
    require(n <= MAX_NAME && n <= r.remaining()) { "bad name" }
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
}
