// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.integration.clipboard

import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFails

class ClipFileTest {
  @Test
  fun fileMimePreservesProviderAndDescriptionTypes() {
    val declared = listOf("application/pdf", "text/plain")
    assertEquals("image/jpeg", ClipFile.resolveMime("image/jpeg", declared, 1))
    assertEquals("application/pdf", ClipFile.resolveMime(null, declared, 1))
    assertEquals("application/pdf", ClipFile.resolveMime(null, listOf("application/pdf"), 2))
    assertEquals("image/jpeg", ClipFile.resolveMime("image/jpeg", declared, 2))
    assertEquals("application/octet-stream", ClipFile.resolveMime(null, declared, 2))
    assertEquals("application/octet-stream", ClipFile.resolveMime(null, emptyList(), 1))
    assertEquals("application/octet-stream", ClipFile.resolveMime(null, emptyList(), 2))
  }

  @Test
  fun fileClipRoundtripAndBounds() {
    val f =
      ClipFiles(
        listOf(
          ClipFile("notes é.txt", "text/plain", "hello".toByteArray()),
          ClipFile("notes é.txt", "application/octet-stream", byteArrayOf(0, -1)),
        ),
      )
    val b = f.encode()
    val decoded = ClipFiles.decode(b)
    assertEquals(f.files.map { it.name }, decoded.files.map { it.name })
    assertEquals(f.files.map { it.mime }, decoded.files.map { it.mime })
    f.files.zip(decoded.files).forEach { (a, b) -> assertContentEquals(a.bytes, b.bytes) }
    for (n in 0 until b.size) assertFails { ClipFiles.decode(b.copyOf(n)) }
    assertFails { ClipFiles.decode(b + byteArrayOf(0)) }
    for (name in listOf("..", "", "../x", "x/y", "x\\y")) {
      assertFails { ClipFiles(listOf(f.files[0].copy(name = name))).encode() }
    }
    assertFails { ClipFiles(listOf(f.files[0].copy(mime = "not a MIME"))).encode() }
    assertFails { ClipFiles.decode(ByteArray((16 shl 20) + 1)) }
    assertFails { ClipFiles(emptyList()).encode() }
    assertFails { ClipFiles(List(ClipFiles.MAX_FILES + 1) { f.files[0] }).encode() }
    assertFails { ClipFiles(List(2) { f.files[0].copy(bytes = ByteArray(9 shl 20)) }).encode() }
    assertFails {
      ClipFiles(listOf(f.files[0].copy(bytes = ByteArray(ClipFile.MAX_BYTES + 1)))).encode()
    }
    val negativeLength = b.clone().apply { fill(0xff.toByte(), 0, 4) }
    assertFails { ClipFiles.decode(negativeLength) }
  }
}
