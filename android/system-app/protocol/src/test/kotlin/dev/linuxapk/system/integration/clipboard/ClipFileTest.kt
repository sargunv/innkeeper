// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.integration.clipboard

import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFails

class ClipFileTest {
  @Test
  fun fileClipRoundtripAndBounds() {
    val f = ClipFile("notes é.txt", "text/plain", "hello".toByteArray())
    val b = f.encode()
    val decoded = ClipFile.decode(b)
    assertEquals(f.name, decoded.name)
    assertEquals(f.mime, decoded.mime)
    assertContentEquals(f.bytes, decoded.bytes)
    for (n in 0 until b.size) assertFails { ClipFile.decode(b.copyOf(n)) }
    assertFails { ClipFile.decode(b + byteArrayOf(0)) }
    for (name in listOf("..", "", "../x", "x/y", "x\\y")) {
      assertFails { f.copy(name = name).encode() }
    }
    assertFails { f.copy(mime = "not a MIME").encode() }
    assertFails { ClipFile.decode(ByteArray((16 shl 20) + 1)) }
    val negativeLength = b.clone().apply { fill(0xff.toByte(), 0, 4) }
    assertFails { ClipFile.decode(negativeLength) }
  }
}
