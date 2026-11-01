// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.a11y

import dev.sargunv.innkeeper.system.a11y.A11yWire.Node
import dev.sargunv.innkeeper.system.bridge.Wire
import java.io.File
import java.nio.ByteBuffer
import java.nio.ByteOrder
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals

/**
 * Tests against host/bridge/src/a11y/snapshot-fixture.txt, which the bridge's `wire::tests` also
 * check.
 */
class A11yWireTest {
  private fun fixtureBytes(): ByteArray =
    File(checkNotNull(System.getProperty("a11yFixture")) { "run through Gradle" })
      .readLines()
      .map(String::trim)
      .filter { it.isNotEmpty() && !it.startsWith("#") }
      .joinToString("")
      .chunked(2)
      .map { it.toInt(16).toByte() }
      .toByteArray()

  @Test
  fun encodesTheSharedFixture() {
    val root = Node(1).apply { bounds(0, 0, 1200, 900).children += 2 }
    val list =
      Node(2).apply {
        flags = A11yWire.ENABLED or A11yWire.VISIBLE_TO_USER
        actions = 1 shl A11yWire.SCROLL_FORWARD
        bounds(0, 0, 1200, 900)
        className = "android.widget.ListView"
        collection = A11yWire.Collection(2, 1, false, 0)
        children += 3
      }
    val edit =
      Node(3).apply {
        flags = A11yWire.ENABLED or A11yWire.EDITABLE or A11yWire.FOCUSED
        actions = (1 shl A11yWire.SET_TEXT) or (1 shl A11yWire.CLICK)
        bounds(10, 20, 300, 80)
        className = "android.widget.EditText"
        text = "héllo"
        hint = "Name"
        viewId = "dev.sargunv.innkeeper.probes:id/name"
        selectionStart = 1
        selectionEnd = 3
        live = 1
        range = A11yWire.Range(2, 0f, 10f, 2.5f)
        item = A11yWire.Item(0, 0, 1, 1, false)
        custom += 7 to "Archive"
      }
    val b = A11yWire.encode(false, 1.5f, 3, "Probes", listOf(root, list, edit))
    assertContentEquals(fixtureBytes(), b)
  }

  @Test
  fun clampsWhatTheBridgeWouldRefuse() {
    val n = Node(1).apply { text = "a\u0000b" + "x".repeat(20_000) }
    val b = A11yWire.encode(false, 99f, -5, "t".repeat(300), listOf(n))
    val r = ByteBuffer.wrap(b).order(ByteOrder.LITTLE_ENDIAN)
    r.short
    r.short
    assertEquals(16f, r.float)
    assertEquals(0L, r.long)
    assertEquals(A11yWire.MAX_TITLE, r.short.toInt())
    r.position(r.position() + A11yWire.MAX_TITLE + 4 + 8 + 4 + 4 + 16)
    r.position(r.position() + 2) // class name ""
    val text = r.short.toInt() and 0xffff
    assertEquals(A11yWire.MAX_TEXT, text)
    val s = ByteArray(text).also { r.get(it) }
    assertEquals("ab", String(s, 0, 2, Charsets.UTF_8))
  }

  @Test
  fun readsActions() {
    // As the bridge's proto::ActionEvent::encode writes one.
    val w = Wire.Writer().u32(4).u64(9).u8(A11yWire.SET_SELECTION).u8(0).u16(0)
    w.i32(2).i32(5).f32(0.5f).line("hé", 400)
    val a = A11yWire.action(ByteBuffer.wrap(w.bytes()).order(ByteOrder.LITTLE_ENDIAN))
    assertEquals(A11yWire.Action(4, 9, A11yWire.SET_SELECTION, 2, 5, 0.5f, "hé"), a)
  }

  private fun Node.bounds(l: Int, t: Int, r: Int, b: Int) = apply {
    bounds[0] = l
    bounds[1] = t
    bounds[2] = r
    bounds[3] = b
  }
}
