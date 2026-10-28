// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.ime

import android.inputmethodservice.InputMethodService
import android.os.Handler
import android.os.Looper
import android.util.Log
import android.view.KeyEvent
import android.view.inputmethod.CursorAnchorInfo
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection
import dev.linuxapk.system.LinuxApkSystemApp
import dev.linuxapk.system.input.HostEvent
import dev.linuxapk.system.input.HostInputClient
import kotlin.math.roundToInt

/**
 * "Linux.apk Keyboard": the host's keyboard and input methods (IBus, Fcitx) for Android text
 * fields, through the bridge's `zwp_text_input_v3` (docs/input.md).
 *
 * It shows no keyboard. Keys still arrive as injected key events, which this service passes on,
 * except those whose character Android's key character map would get wrong under the host layout
 * (accented letters, dead keys, Compose): for those it commits the host's text instead
 * ([HostInputClient.keyText]). For each editor it tells the bridge to enable text input (with the
 * content type), and keeps the bridge up to date with the surrounding text and the cursor
 * rectangle; the host input method's preedit, commit and delete come back as `ti_*` lines from the
 * bridge's input service and are applied here through the `InputConnection`.
 */
class HostImeService : InputMethodService(), HostInputClient.Listener {
  private val main = Handler(Looper.getMainLooper())
  private val session = TextInputSession()
  private var active = false
  private val pts = FloatArray(4)
  private val committedKeys = HashSet<Int>() // scan codes whose down committed text instead

  private val input
    get() = (application as LinuxApkSystemApp).input

  private fun send(vararg lines: String) {
    for (l in lines) if (!input.textInput(l)) return
  }

  override fun onCreate() {
    super.onCreate()
    input.addListener(this)
    Log.i(TAG, "created")
  }

  override fun onDestroy() {
    input.removeListener(this)
    super.onDestroy()
  }

  // No on-screen keyboard, ever: the host keyboard types.
  override fun onEvaluateInputViewShown() = false

  override fun onShowInputRequested(flags: Int, configChange: Boolean) = false

  override fun onEvaluateFullscreenMode() = false

  override fun onStartInput(attribute: EditorInfo, restarting: Boolean) {
    super.onStartInput(attribute, restarting)
    session.reset()
    active = TextInputSession.wantsText(attribute.inputType)
    if (!active) {
      send("ti_disable", "ti_apply")
      return
    }
    val ct = TextInputSession.contentType(attribute.inputType, attribute.imeOptions)
    val lines = session.enableLines(ct).toMutableList()
    val ic = currentInputConnection
    surrounding(ic, attribute.initialSelStart, attribute.initialSelEnd)?.let { lines += it }
    lines += "ti_apply"
    send(*lines.toTypedArray())
    ic?.requestCursorUpdates(
      InputConnection.CURSOR_UPDATE_MONITOR or InputConnection.CURSOR_UPDATE_IMMEDIATE,
    )
  }

  override fun onFinishInput() {
    if (active) send("ti_disable", "ti_apply")
    active = false
    session.reset()
    super.onFinishInput()
  }

  override fun onUpdateSelection(
    oldSelStart: Int,
    oldSelEnd: Int,
    newSelStart: Int,
    newSelEnd: Int,
    candidatesStart: Int,
    candidatesEnd: Int,
  ) {
    super.onUpdateSelection(
      oldSelStart,
      oldSelEnd,
      newSelStart,
      newSelEnd,
      candidatesStart,
      candidatesEnd,
    )
    if (!active) return
    surrounding(currentInputConnection, newSelStart, newSelEnd)?.let { send(it, "ti_apply") }
  }

  override fun onUpdateCursorAnchorInfo(info: CursorAnchorInfo) {
    if (!active) return
    val x = info.insertionMarkerHorizontal
    val top = info.insertionMarkerTop
    val bottom = info.insertionMarkerBottom
    if (x.isNaN() || top.isNaN() || bottom.isNaN()) return
    // Local coordinates to the display's ("screen") pixels.
    val matrix = info.matrix
    pts[0] = x
    pts[1] = top
    pts[2] = x
    pts[3] = bottom
    matrix.mapPoints(pts)
    val h = (pts[3] - pts[1]).roundToInt().coerceAtLeast(1)
    send("ti_cursor ${pts[0].roundToInt()} ${pts[1].roundToInt()} 1 $h", "ti_apply")
  }

  // Keys reach the IME before the app, in order, so committing here keeps typing order.
  override fun onKeyDown(keyCode: Int, event: KeyEvent): Boolean {
    val ic = currentInputConnection
    val text = if (ic != null) input.keyText(event) else null
    if (ic == null || text == null) return super.onKeyDown(keyCode, event)
    // Repeats (InputDispatcher's) commit again; a dead key commits nothing.
    if (text.isNotEmpty()) ic.commitText(text, 1)
    committedKeys += event.scanCode
    return true
  }

  override fun onKeyUp(keyCode: Int, event: KeyEvent): Boolean {
    input.keyTextDone(event)
    if (committedKeys.remove(event.scanCode)) return true
    return super.onKeyUp(keyCode, event)
  }

  /** The surrounding-text line for the editor's current state, or null if it can't be read. */
  private fun surrounding(ic: InputConnection?, selStart: Int, selEnd: Int): String? {
    ic ?: return null
    val st = ic.getSurroundingText(CONTEXT, CONTEXT, 0)
    if (st != null) return session.surroundingLine(st.text, st.selectionStart, st.selectionEnd)
    if (selStart < 0) return null
    return session.surroundingLine("", 0, 0)
  }

  // Host events arrive on the input thread; InputConnection calls belong on the main thread.
  override fun onHostEvent(e: HostEvent) {
    when (e) {
      is HostEvent.TextDelete,
      is HostEvent.TextCommit,
      is HostEvent.TextPreedit,
      is HostEvent.TextDone -> main.post { onTextEvent(e) }
      else -> {}
    }
  }

  private fun onTextEvent(e: HostEvent) {
    val ic = currentInputConnection
    session.onEvent(e, if (active && ic != null) Editor(ic) else null)
    if (e is HostEvent.TextDone) Log.d(TAG, "applied text input for display ${e.display}")
  }

  private class Editor(val ic: InputConnection) : TextEditor {
    override fun beginBatchEdit() {
      ic.beginBatchEdit()
    }

    override fun endBatchEdit() {
      ic.endBatchEdit()
    }

    override fun setComposingText(text: String, newCursorPosition: Int) {
      ic.setComposingText(text, newCursorPosition)
    }

    override fun commitText(text: String, newCursorPosition: Int) {
      ic.commitText(text, newCursorPosition)
    }

    override fun deleteSurroundingTextInCodePoints(before: Int, after: Int) {
      ic.deleteSurroundingTextInCodePoints(before, after)
    }
  }

  companion object {
    const val TAG = "LinuxApkIme"
    private const val CONTEXT = 200
  }
}
