// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.ime

import android.text.InputType
import dev.sargunv.innkeeper.system.input.HostEvent
import dev.sargunv.innkeeper.system.input.PercentText
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull
import kotlin.test.assertTrue

class TextInputSessionTest {
  /** Records calls and keeps a model of the text, like a real editor. */
  private class FakeEditor(var text: String, var cursor: Int) : TextEditor {
    val calls = mutableListOf<String>()
    var compStart = -1
    var compEnd = -1
    var batch = 0

    override fun beginBatchEdit() {
      batch++
    }

    override fun endBatchEdit() {
      batch--
    }

    private fun replaceComposing(t: String, newCursor: Int) {
      val (s, e) = if (compStart >= 0) compStart to compEnd else cursor to cursor
      text = text.substring(0, s) + t + text.substring(e)
      cursor = if (newCursor > 0) s + t.length + newCursor - 1 else s + newCursor
      if (t.isEmpty()) {
        compStart = -1
        compEnd = -1
      } else {
        compStart = s
        compEnd = s + t.length
      }
    }

    override fun setComposingText(text: String, newCursorPosition: Int) {
      calls += "compose '$text' $newCursorPosition"
      replaceComposing(text, newCursorPosition)
    }

    override fun commitText(text: String, newCursorPosition: Int) {
      calls += "commit '$text'"
      replaceComposing(text, newCursorPosition)
      compStart = -1
      compEnd = -1
    }

    override fun deleteSurroundingTextInCodePoints(before: Int, after: Int) {
      calls += "delete $before $after"
      val b = text.offsetByCodePoints(cursor, -before)
      val a = text.offsetByCodePoints(cursor, after)
      text = text.substring(0, b) + text.substring(a)
      cursor = b
    }
  }

  private fun feed(s: TextInputSession, ed: TextEditor, vararg lines: String) {
    for (l in lines) s.onEvent(HostEvent.parse(l) ?: error("unparsed: $l"), ed)
  }

  @Test
  fun pinyinPreeditThenCommit() {
    val s = TextInputSession()
    val ed = FakeEditor("", 0)
    feed(s, ed, "ti_preedit 1 1 n", "ti_done 3")
    feed(s, ed, "ti_preedit 2 2 ni", "ti_done 3")
    assertEquals("ni", ed.text)
    assertTrue(s.composing)
    feed(s, ed, "ti_commit 你", "ti_preedit -1 -1", "ti_done 3")
    assertEquals("你", ed.text)
    assertEquals(1, ed.cursor)
    assertTrue(!s.composing)
    assertEquals(0, ed.batch)
  }

  @Test
  fun deadKeyComposeClearsPreedit() {
    val s = TextInputSession()
    val ed = FakeEditor("caf", 3)
    feed(s, ed, "ti_preedit 2 2 ´", "ti_done 2")
    assertEquals("caf´", ed.text)
    feed(s, ed, "ti_commit é", "ti_preedit -1 -1", "ti_done 2")
    assertEquals("café", ed.text)
    // A preedit that just goes away (Escape) removes the composing text.
    feed(s, ed, "ti_preedit 1 1 x", "ti_done 2")
    feed(s, ed, "ti_preedit -1 -1", "ti_done 2")
    assertEquals("café", ed.text)
  }

  @Test
  fun deleteConvertsBytesToCodePoints() {
    val s = TextInputSession()
    val text = "a日本😀b"
    val cursor = text.indexOf('b')
    val ed = FakeEditor(text, cursor)
    val line = s.surroundingLine(text, cursor, cursor)
    // cursor after "a日本😀": 1 + 3 + 3 + 4 bytes
    assertEquals("ti_surrounding 11 11 1 a日本😀b", line)
    // Delete "本😀" (7 bytes) and replace it.
    feed(s, ed, "ti_delete 7 0", "ti_commit x", "ti_done 1")
    assertEquals("a日xb", ed.text)
    assertEquals(listOf("delete 2 0", "commit 'x'"), ed.calls)
    // The selection update after that edit is the input method's own (cause 0), and later ones
    // are not.
    assertEquals("ti_surrounding 5 5 0 a日xb", s.surroundingLine(ed.text, 3, 3))
    assertEquals("ti_surrounding 5 5 1 a日xb", s.surroundingLine(ed.text, 3, 3))
  }

  @Test
  fun surroundingIsTrimmedToFit() {
    val s = TextInputSession()
    val text = "é".repeat(1000) + "|" + "%".repeat(1000)
    val c = 1000
    val line = s.surroundingLine(text, c, c)
    assertTrue(line.toByteArray().size <= TextInputSession.MAX_LINE, "${line.length}")
    val f = line.split(' ')
    val decoded = PercentText.decode(f[4])!!
    val cur = f[1].toInt()
    assertEquals(cur, f[2].toInt())
    assertEquals("1", f[3])
    assertEquals('|', decoded.toByteArray().decodeToString(cur, cur + 1)[0])
    // A selection too big to send at all sends the cursor without text.
    assertEquals("ti_surrounding 0 0 1 ", s.surroundingLine("%".repeat(400), 0, 400))
  }

  @Test
  fun percentTextMatchesTheBridge() {
    assertEquals("a%20b%25c%0A日", PercentText.encode("a b%c\n日"))
    assertEquals("a b%c\n日", PercentText.decode("a%20b%25c%0A日"))
    assertNull(PercentText.decode("%2"))
    assertNull(PercentText.decode("%ZZ"))
    assertNull(PercentText.decode("%FF"))
  }

  @Test
  fun contentTypes() {
    fun ct(t: Int, o: Int = 0) = TextInputSession.contentType(t, o)
    val text = InputType.TYPE_CLASS_TEXT
    assertEquals(
      TextInputSession.ContentType(
        TextInputSession.HINT_HIDDEN_TEXT or TextInputSession.HINT_SENSITIVE_DATA,
        TextInputSession.PURPOSE_PASSWORD,
      ),
      ct(text or InputType.TYPE_TEXT_VARIATION_PASSWORD),
    )
    assertEquals(
      TextInputSession.PURPOSE_EMAIL,
      ct(text or InputType.TYPE_TEXT_VARIATION_EMAIL_ADDRESS).purpose,
    )
    assertEquals(
      TextInputSession.HINT_MULTILINE or TextInputSession.HINT_AUTO_CAPITALIZATION,
      ct(text or InputType.TYPE_TEXT_FLAG_MULTI_LINE or InputType.TYPE_TEXT_FLAG_CAP_SENTENCES)
        .hint,
    )
    assertEquals(TextInputSession.PURPOSE_DIGITS, ct(InputType.TYPE_CLASS_NUMBER).purpose)
    assertEquals(
      TextInputSession.PURPOSE_PIN,
      ct(InputType.TYPE_CLASS_NUMBER or InputType.TYPE_NUMBER_VARIATION_PASSWORD).purpose,
    )
    assertEquals(TextInputSession.PURPOSE_PHONE, ct(InputType.TYPE_CLASS_PHONE).purpose)
    assertTrue(!TextInputSession.wantsText(InputType.TYPE_NULL))
    assertTrue(TextInputSession.wantsText(text))
  }
}
