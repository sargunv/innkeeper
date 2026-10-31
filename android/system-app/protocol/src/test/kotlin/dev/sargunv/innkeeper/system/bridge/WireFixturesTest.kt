// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.bridge

import java.io.File
import java.nio.ByteBuffer
import java.nio.ByteOrder
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertTrue

/**
 * host/bridge/src/ipc/wire-fixtures.txt, which the bridge's tests also check: each message built
 * from its fields must match the hex, and read back.
 */
class WireFixturesTest {
  private class Case(val fields: List<Pair<String, String>>, val bytes: ByteArray)

  private fun cases(): List<Case> {
    val lines =
      File(checkNotNull(System.getProperty("wireFixtures")) { "run through Gradle" })
        .readLines()
        .map(String::trim)
        .filter {
          it.isNotEmpty() && !it.startsWith("#")
        }
    return lines.chunked(2).map { (fields, hex) ->
      Case(
        fields.split("; ").map { it.substringBefore(' ') to it.substringAfter(' ', "") },
        hex.chunked(2).map { it.toInt(16).toByte() }.toByteArray(),
      )
    }
  }

  private fun int(v: String): Long =
    if (v.startsWith("0x")) v.removePrefix("0x").toLong(16) else v.toLong()

  private fun text(v: String) = v.removeSurrounding("\"")

  @Test
  fun encodesLikeTheBridge() {
    val cases = cases()
    assertTrue(cases.isNotEmpty())
    for (c in cases) {
      var w = Wire.Writer()
      for ((op, v) in c.fields) {
        when (op) {
          "message" -> {
            val (type, tag) = v.split(' ').map(::int)
            w = Wire.message(type.toInt(), tag.toInt())
          }
          "u8" -> w.u8(int(v).toInt())
          "u16" -> w.u16(int(v).toInt())
          "u32" -> w.u32(int(v).toInt())
          "i32" -> w.i32(int(v).toInt())
          "u64" -> w.u64(int(v))
          "f32" -> w.f32(v.toFloat())
          "name" -> w.name(text(v))
          else -> error("unknown field $op")
        }
      }
      assertEquals(c.bytes.toList(), w.bytes().toList(), "${c.fields}")
    }
  }

  @Test
  fun decodesLikeTheBridge() {
    for (c in cases()) {
      val m = assertNotNull(Wire.parse(c.bytes, c.bytes.size))
      val r = m.body
      for ((op, v) in c.fields) {
        when (op) {
          "message" -> assertEquals(v, "0x%04x %d".format(m.type, m.tag))
          "u8" -> assertEquals(int(v), r.get().toLong() and 0xff)
          "u16" -> assertEquals(int(v), r.short.toLong() and 0xffff)
          "u32" -> assertEquals(int(v), r.int.toLong() and 0xffffffffL)
          "i32" -> assertEquals(int(v), r.int.toLong())
          "u64" -> assertEquals(int(v), r.long)
          "f32" -> assertEquals(v.toFloat(), r.float)
          "name" ->
            assertEquals(
              String(Wire.truncateUtf8(text(v), Wire.MAX_NAME), Charsets.UTF_8),
              Wire.name(r),
            )
        }
      }
      assertEquals(0, r.remaining(), "${c.fields}: trailing bytes")
    }
  }

  @Test
  fun refusesPacketsOfTheWrongSize() {
    assertEquals(null, Wire.parse(ByteArray(Wire.HDR - 1), Wire.HDR - 1))
    assertEquals(null, Wire.parse(ByteArray(Wire.MAX_MSG + 1), Wire.MAX_MSG + 1))
    val longName = ByteBuffer.wrap(ByteArray(100)).order(ByteOrder.LITTLE_ENDIAN)
    longName.putShort(0, (Wire.MAX_NAME + 1).toShort())
    assertTrue(runCatching { Wire.name(longName) }.isFailure)
  }
}
