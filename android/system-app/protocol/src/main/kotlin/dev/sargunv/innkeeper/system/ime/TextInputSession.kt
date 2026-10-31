// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.ime

import android.text.InputType
import android.view.inputmethod.EditorInfo
import dev.sargunv.innkeeper.system.input.HostEvent
import dev.sargunv.innkeeper.system.input.PercentText

/** The `InputConnection` calls the IME makes; an interface so the logic runs on a plain JVM. */
interface TextEditor {
  fun beginBatchEdit()

  fun endBatchEdit()

  fun setComposingText(text: String, newCursorPosition: Int)

  fun commitText(text: String, newCursorPosition: Int)

  fun deleteSurroundingTextInCodePoints(before: Int, after: Int)
}

/**
 * The Innkeeper IME's side of `zwp_text_input_v3` (docs/input.md), without Android types: it builds
 * the lines for the bridge (`ti_*`, parsed by the bridge's `input/ime.rs`) and applies the host
 * input method's results to a [TextEditor].
 */
class TextInputSession {
  /** The surrounding text last sent: what the bridge's byte offsets refer to. */
  private var sentText = ""
  private var sentSelStart = 0 // UTF-16 index into sentText
  private var sentSelEnd = 0

  private var delete: HostEvent.TextDelete? = null
  private val commit = StringBuilder()
  private var hasCommit = false
  private var preedit: HostEvent.TextPreedit? = null

  /** A preedit is shown in the editor (as composing text). */
  var composing = false
    private set

  /** A new editor: forget the old one's state. */
  fun reset() {
    sentText = ""
    sentSelStart = 0
    sentSelEnd = 0
    delete = null
    commit.clear()
    hasCommit = false
    preedit = null
    composing = false
    ownEdit = false
  }

  /** Lines that enable text input for an editor. */
  fun enableLines(info: ContentType): List<String> =
    listOf("ti_enable", "ti_content ${info.hint} ${info.purpose}")

  /** Set when a batch changed the text: the next selection update is the input method's doing. */
  private var ownEdit = false

  /**
   * The `ti_surrounding` line for [text] with the selection [selStart]..[selEnd] (UTF-16 indices),
   * trimmed around the selection until the line fits. The change counts as the input method's if it
   * follows one of its edits, else as "other" (which resets the host input method).
   */
  fun surroundingLine(text: CharSequence, selStart: Int, selEnd: Int): String {
    val cause = if (ownEdit) 0 else 1
    ownEdit = false
    val lo = minOf(selStart, selEnd).coerceIn(0, text.length)
    val hi = maxOf(selStart, selEnd).coerceIn(lo, text.length)
    var keep = MAX_CONTEXT
    while (true) {
      var from = (lo - keep).coerceAtLeast(0)
      var to = (hi + keep).coerceAtMost(text.length)
      // Never split a surrogate pair.
      if (from in 1 until lo && Character.isLowSurrogate(text[from])) from++
      if (to in (hi + 1)..text.length && Character.isHighSurrogate(text[to - 1])) to--
      val t = text.subSequence(from, to).toString()
      val start = lo - from
      val end = hi - from
      val line =
        "ti_surrounding ${utf8(t.substring(0, end))} ${utf8(t.substring(0, start))} $cause " +
          PercentText.encode(t)
      if (line.toByteArray(Charsets.UTF_8).size <= MAX_LINE) {
        sentText = t
        sentSelStart = start
        sentSelEnd = end
        return line
      }
      if (keep == 0) {
        // The selection alone doesn't fit: send no text, just the cursor.
        sentText = ""
        sentSelStart = 0
        sentSelEnd = 0
        return "ti_surrounding 0 0 $cause "
      }
      keep /= 2
    }
  }

  /** Gathers one event; on [HostEvent.TextDone], applies the batch to [editor]. */
  fun onEvent(e: HostEvent, editor: TextEditor?) {
    when (e) {
      is HostEvent.TextDelete -> delete = e
      is HostEvent.TextCommit -> {
        commit.append(e.text)
        hasCommit = true
      }
      is HostEvent.TextPreedit -> preedit = e
      is HostEvent.TextDone -> {
        if (editor != null) apply(editor)
        delete = null
        commit.clear()
        hasCommit = false
        preedit = null
      }
      else -> {}
    }
  }

  // zwp_text_input_v3's order: remove the preedit, delete around the cursor, insert the commit,
  // show the new preedit.
  private fun apply(ed: TextEditor) {
    ownEdit = delete != null || hasCommit || preedit?.text?.isNotEmpty() == true || composing
    ed.beginBatchEdit()
    try {
      delete?.let { d ->
        if (composing) {
          ed.setComposingText("", 1)
          composing = false
        }
        val (b, a) = codePoints(d.before, d.after)
        if (b > 0 || a > 0) ed.deleteSurroundingTextInCodePoints(b, a)
      }
      if (hasCommit) {
        // Replaces the composing text, if any.
        ed.commitText(commit.toString(), 1)
        composing = false
      }
      val p = preedit
      if (p != null && p.text.isNotEmpty()) {
        // setComposingText can only put the cursor at either end of the text.
        ed.setComposingText(p.text, if (p.end == 0) 0 else 1)
        composing = true
      } else if (composing) {
        ed.setComposingText("", 1)
        composing = false
      }
    } finally {
      ed.endBatchEdit()
    }
  }

  /** The bridge's byte lengths, as code points of the text around the selection last sent. */
  private fun codePoints(beforeBytes: Int, afterBytes: Int): Pair<Int, Int> {
    var n = 0
    var bytes = 0
    var i = sentSelStart.coerceIn(0, sentText.length)
    while (i > 0 && bytes < beforeBytes) {
      val cp = Character.codePointBefore(sentText, i)
      i -= Character.charCount(cp)
      bytes += utf8Len(cp)
      n++
    }
    val before = n + (beforeBytes - bytes).coerceAtLeast(0) // past what we sent: assume ASCII
    n = 0
    bytes = 0
    i = sentSelEnd.coerceIn(0, sentText.length)
    while (i < sentText.length && bytes < afterBytes) {
      val cp = Character.codePointAt(sentText, i)
      i += Character.charCount(cp)
      bytes += utf8Len(cp)
      n++
    }
    val after = n + (afterBytes - bytes).coerceAtLeast(0)
    return before to after
  }

  /** zwp_text_input_v3 content hint bits and purpose. */
  data class ContentType(val hint: Int, val purpose: Int)

  companion object {
    const val MAX_LINE = 470
    private const val MAX_CONTEXT = 120 // UTF-16 units each side of the selection

    // zwp_text_input_v3.content_hint
    const val HINT_COMPLETION = 0x1
    const val HINT_SPELLCHECK = 0x2
    const val HINT_AUTO_CAPITALIZATION = 0x4
    const val HINT_UPPERCASE = 0x10
    const val HINT_TITLECASE = 0x20
    const val HINT_HIDDEN_TEXT = 0x40
    const val HINT_SENSITIVE_DATA = 0x80
    const val HINT_MULTILINE = 0x200

    // zwp_text_input_v3.content_purpose
    const val PURPOSE_NORMAL = 0
    const val PURPOSE_DIGITS = 2
    const val PURPOSE_NUMBER = 3
    const val PURPOSE_PHONE = 4
    const val PURPOSE_URL = 5
    const val PURPOSE_EMAIL = 6
    const val PURPOSE_NAME = 7
    const val PURPOSE_PASSWORD = 8
    const val PURPOSE_PIN = 9
    const val PURPOSE_DATE = 10
    const val PURPOSE_TIME = 11
    const val PURPOSE_DATETIME = 12

    /** Whether an editor with this input type wants text input (TYPE_NULL: raw keys only). */
    fun wantsText(inputType: Int) = inputType and InputType.TYPE_MASK_CLASS != InputType.TYPE_NULL

    /** Maps `EditorInfo.inputType` and `imeOptions` to a content type. */
    fun contentType(inputType: Int, imeOptions: Int): ContentType {
      val cls = inputType and InputType.TYPE_MASK_CLASS
      val variation = inputType and InputType.TYPE_MASK_VARIATION
      val flags = inputType and InputType.TYPE_MASK_FLAGS
      var hint = 0
      var purpose = PURPOSE_NORMAL
      when (cls) {
        InputType.TYPE_CLASS_TEXT -> {
          when (variation) {
            InputType.TYPE_TEXT_VARIATION_URI -> purpose = PURPOSE_URL
            InputType.TYPE_TEXT_VARIATION_EMAIL_ADDRESS,
            InputType.TYPE_TEXT_VARIATION_WEB_EMAIL_ADDRESS -> purpose = PURPOSE_EMAIL
            InputType.TYPE_TEXT_VARIATION_PERSON_NAME -> purpose = PURPOSE_NAME
            InputType.TYPE_TEXT_VARIATION_PASSWORD,
            InputType.TYPE_TEXT_VARIATION_WEB_PASSWORD -> {
              purpose = PURPOSE_PASSWORD
              hint = hint or HINT_HIDDEN_TEXT or HINT_SENSITIVE_DATA
            }
            InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD -> {
              purpose = PURPOSE_PASSWORD
              hint = hint or HINT_SENSITIVE_DATA
            }
          }
          if (flags and InputType.TYPE_TEXT_FLAG_CAP_CHARACTERS != 0) hint = hint or HINT_UPPERCASE
          if (flags and InputType.TYPE_TEXT_FLAG_CAP_WORDS != 0) hint = hint or HINT_TITLECASE
          if (flags and InputType.TYPE_TEXT_FLAG_CAP_SENTENCES != 0) {
            hint = hint or HINT_AUTO_CAPITALIZATION
          }
          if (flags and InputType.TYPE_TEXT_FLAG_AUTO_CORRECT != 0) hint = hint or HINT_SPELLCHECK
          if (flags and InputType.TYPE_TEXT_FLAG_AUTO_COMPLETE != 0) {
            hint = hint or HINT_COMPLETION
          }
          if (flags and InputType.TYPE_TEXT_FLAG_MULTI_LINE != 0) hint = hint or HINT_MULTILINE
          if (flags and InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS != 0) {
            hint = hint and (HINT_COMPLETION or HINT_SPELLCHECK).inv()
          }
        }
        InputType.TYPE_CLASS_NUMBER ->
          purpose =
            when {
              variation == InputType.TYPE_NUMBER_VARIATION_PASSWORD -> {
                hint = hint or HINT_HIDDEN_TEXT or HINT_SENSITIVE_DATA
                PURPOSE_PIN
              }
              flags and (InputType.TYPE_NUMBER_FLAG_DECIMAL or InputType.TYPE_NUMBER_FLAG_SIGNED) !=
                0 -> PURPOSE_NUMBER
              else -> PURPOSE_DIGITS
            }
        InputType.TYPE_CLASS_PHONE -> purpose = PURPOSE_PHONE
        InputType.TYPE_CLASS_DATETIME ->
          purpose =
            when (variation) {
              InputType.TYPE_DATETIME_VARIATION_DATE -> PURPOSE_DATE
              InputType.TYPE_DATETIME_VARIATION_TIME -> PURPOSE_TIME
              else -> PURPOSE_DATETIME
            }
      }
      if (imeOptions and EditorInfo.IME_FLAG_NO_PERSONALIZED_LEARNING != 0) {
        hint = hint or HINT_SENSITIVE_DATA
      }
      return ContentType(hint, purpose)
    }

    private fun utf8Len(cp: Int) =
      when {
        cp < 0x80 -> 1
        cp < 0x800 -> 2
        cp < 0x10000 -> 3
        else -> 4
      }

    private fun utf8(s: String) = s.toByteArray(Charsets.UTF_8).size
  }
}
