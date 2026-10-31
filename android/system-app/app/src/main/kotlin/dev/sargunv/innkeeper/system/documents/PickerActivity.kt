// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.documents

import android.app.Activity
import android.content.ClipData
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.provider.DocumentsContract
import android.util.Log
import android.view.KeyEvent

/**
 * `ACTION_OPEN_DOCUMENT`, `GET_CONTENT`, `CREATE_DOCUMENT` and `OPEN_DOCUMENT_TREE`, answered with
 * the desktop's file chooser (the FileChooser portal, parented to the app's window). Its intent
 * filters outrank DocumentsUI's (priority 200 against 100; honoured for privileged apps). The
 * results are [HostDocumentsProvider] URIs. Without the bridge the request goes on to DocumentsUI.
 *
 * Grants follow DocumentsUI: read for GET_CONTENT; read, write and persistable for OPEN_DOCUMENT
 * and CREATE_DOCUMENT; plus prefix for a folder.
 */
class PickerActivity : Activity() {
  private val state = PickerState()
  private var listener: HostFiles.PickListener? = null
  private val main = Handler(Looper.getMainLooper())
  private var waiting: (() -> Unit)? = null
  private val timeout = Runnable {
    stopWaiting()
    fallback()
  }

  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    if (savedInstanceState != null) {
      // The process restarted under us; the pick is gone.
      finish()
      return
    }
    val action = intent.action
    val multiple = intent.getBooleanExtra(Intent.EXTRA_ALLOW_MULTIPLE, false)
    val mode =
      when (action) {
        Intent.ACTION_OPEN_DOCUMENT,
        Intent.ACTION_GET_CONTENT ->
          if (multiple) HostFiles.MODE_OPEN_MULTIPLE else HostFiles.MODE_OPEN
        Intent.ACTION_CREATE_DOCUMENT -> HostFiles.MODE_SAVE
        Intent.ACTION_OPEN_DOCUMENT_TREE -> HostFiles.MODE_FOLDER
        else -> return finish()
      }
    val write = action != Intent.ACTION_GET_CONTENT
    val mimes = buildList {
      intent.type?.let(::add)
      intent.getStringArrayExtra(Intent.EXTRA_MIME_TYPES)?.let(::addAll)
    }
    val name =
      if (mode == HostFiles.MODE_SAVE) intent.getStringExtra(Intent.EXTRA_TITLE).orEmpty() else ""
    val displayId = display?.displayId ?: 0
    val pickListener =
      object : HostFiles.PickListener {
        override fun onDone(status: Int, items: List<HostFiles.Picked>) {
          main.post { deliver(mode, write, status, items) }
        }
      }
    listener = pickListener
    val ready: () -> Unit = {
      main.post {
        // The immediate connected check can race the HELLO callback. Only the first wins.
        if (!isFinishing && state.connected()) {
          stopWaiting()
          // PICK waits for an acknowledgement; keep that wait off the UI thread too.
          Thread(
              {
                val ok = HostFiles.pick(mode, write, displayId, "", name, mimes, pickListener)
                main.post {
                  if (state.done) HostFiles.abandon(pickListener) else if (!ok) fallback()
                }
              },
              "innkeeper-picker",
            )
            .start()
        }
      }
      Unit
    }
    waiting = ready
    main.postDelayed(timeout, CONNECT_TIMEOUT_MS)
    HostFiles.whenConnected(ready)
  }

  private fun stopWaiting() {
    waiting?.let(HostFiles::stopWaiting)
    waiting = null
    main.removeCallbacks(timeout)
  }

  private fun fallback() {
    if (isFinishing || !state.finish()) return
    Log.w(HostFiles.TAG, "no desktop file chooser; handing ${intent.action} to DocumentsUI")
    try {
      startActivity(
        Intent(intent)
          .setComponent(null)
          .setPackage(DOCUMENTS_UI)
          .addFlags(Intent.FLAG_ACTIVITY_FORWARD_RESULT),
      )
    } catch (e: Exception) {
      Log.w(HostFiles.TAG, "DocumentsUI: $e")
      setResult(RESULT_CANCELED)
    }
    finish()
  }

  private fun deliver(mode: Int, write: Boolean, status: Int, items: List<HostFiles.Picked>) {
    if (isFinishing || !state.finish()) return
    if (status != 0 || items.isEmpty()) {
      Log.i(HostFiles.TAG, "pick ended: $status")
      setResult(RESULT_CANCELED)
      finish()
      return
    }
    val auth = HostDocumentsProvider.AUTHORITY
    val uris = items.map {
      val id = HostDocumentsProvider.documentId(it.grant)
      if (mode == HostFiles.MODE_FOLDER) DocumentsContract.buildTreeDocumentUri(auth, id)
      else DocumentsContract.buildDocumentUri(auth, id)
    }
    var flags = Intent.FLAG_GRANT_READ_URI_PERMISSION
    if (intent.action != Intent.ACTION_GET_CONTENT) {
      flags = flags or Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION
    }
    if (write && items.all { it.writable }) flags = flags or Intent.FLAG_GRANT_WRITE_URI_PERMISSION
    if (mode == HostFiles.MODE_FOLDER) flags = flags or Intent.FLAG_GRANT_PREFIX_URI_PERMISSION
    val result = Intent().setData(uris[0]).addFlags(flags)
    if (uris.size > 1 || mode == HostFiles.MODE_OPEN_MULTIPLE) {
      val clip = ClipData.newRawUri(null, uris[0])
      uris.drop(1).forEach { clip.addItem(ClipData.Item(it)) }
      result.clipData = clip
    }
    Log.i(HostFiles.TAG, "picked ${uris.size}: ${uris.joinToString(transform = Uri::toString)}")
    setResult(RESULT_OK, result)
    finish()
  }

  // The window shows nothing, but it has focus while the dialog is up: Escape or Back gives up
  // (the desktop dialog stays until the user closes it; its answer is then ignored).
  override fun onKeyDown(keyCode: Int, event: KeyEvent): Boolean {
    if (keyCode == KeyEvent.KEYCODE_ESCAPE || keyCode == KeyEvent.KEYCODE_BACK) {
      cancel()
      return true
    }
    return super.onKeyDown(keyCode, event)
  }

  @Deprecated("Activity.onBackPressed") override fun onBackPressed() = cancel()

  private fun cancel() {
    if (!state.finish()) return
    stopWaiting()
    listener?.let(HostFiles::abandon)
    setResult(RESULT_CANCELED)
    finish()
  }

  override fun onDestroy() {
    state.finish()
    stopWaiting()
    listener?.let(HostFiles::abandon)
    super.onDestroy()
  }

  companion object {
    private const val CONNECT_TIMEOUT_MS = 1_000L
    private const val DOCUMENTS_UI = "com.android.documentsui"
  }
}
