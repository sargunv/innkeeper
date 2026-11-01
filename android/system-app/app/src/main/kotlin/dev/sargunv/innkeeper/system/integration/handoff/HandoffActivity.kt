// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.integration.handoff

import android.app.Activity
import android.app.AlertDialog
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.os.ParcelFileDescriptor
import android.provider.OpenableColumns
import android.util.Log
import android.widget.Toast
import dev.sargunv.innkeeper.system.bridge.Blob
import dev.sargunv.innkeeper.system.bridge.Wire
import dev.sargunv.innkeeper.system.integration.IntegrationFeature
import dev.sargunv.innkeeper.system.integration.browser.UrlOpener
import java.io.File

/** Lowest-priority VIEW fallback, and the Android sharesheet's Desktop target. */
class HandoffActivity : Activity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    if (savedInstanceState != null) return finish()
    val streams =
      when (intent.action) {
        Intent.ACTION_VIEW ->
          listOfNotNull(intent.data?.takeIf { it.scheme in listOf("content", "file") })
        Intent.ACTION_SEND ->
          listOfNotNull(intent.getParcelableExtra(Intent.EXTRA_STREAM, Uri::class.java))
        Intent.ACTION_SEND_MULTIPLE ->
          intent.getParcelableArrayListExtra(Intent.EXTRA_STREAM, Uri::class.java).orEmpty()
        else -> return finish()
      }
    if (streams.size > MAX_FILES) return fail("Share up to $MAX_FILES files at a time")
    if (intent.action == Intent.ACTION_VIEW) {
      val uri = intent.data ?: return finish()
      if (streams.isEmpty()) {
        val ascii = UrlOpener.toAscii(uri.toString()) ?: return fail("Couldn't open the link")
        submit(URI, false, emptyList(), ascii)
      } else submit(OPEN, false, streams, "")
      return
    }
    val text = intent.getCharSequenceExtra(Intent.EXTRA_TEXT)?.toString().orEmpty()
    if (streams.isEmpty() && text.isEmpty()) return fail("Nothing to share")
    val labels = mutableListOf("Open with desktop app", "Save to file…")
    val actions = mutableListOf(OPEN, SAVE)
    if (streams.isEmpty() && text.isNotEmpty()) {
      labels.add("Copy")
      actions.add(COPY)
    }
    labels.add("Email")
    actions.add(EMAIL)
    AlertDialog.Builder(this)
      .setTitle("Share to desktop")
      .setItems(labels.toTypedArray()) { _, which ->
        val action = actions[which]
        if (action == COPY) {
          getSystemService(ClipboardManager::class.java)
            .setPrimaryClip(ClipData.newPlainText("Shared text", text))
          // ClipboardSync forwards this text to the desktop clipboard.
          finish()
        } else submit(action, action == OPEN, streams, text)
      }
      .setOnCancelListener { finish() }
      .show()
  }

  private fun submit(action: Int, ask: Boolean, streams: List<Uri>, text: String) {
    val window = display?.displayId ?: 0
    Thread(
        {
          val copies = mutableListOf<File>()
          val descriptors = mutableListOf<ParcelFileDescriptor>()
          var metadata: java.io.FileDescriptor? = null
          try {
            require(text.toByteArray(Charsets.UTF_8).size <= MAX_TEXT) {
              "Shared text is too large"
            }
            val names = mutableListOf<String>()
            for (uri in streams) {
              val name = displayName(uri)
              names.add(name)
              val copy = File.createTempFile("handoff-", null, cacheDir)
              copies.add(copy)
              contentResolver.openInputStream(uri)?.use { input ->
                copy.outputStream().use { output ->
                  val buffer = ByteArray(64 * 1024)
                  var total = 0L
                  while (true) {
                    val n = input.read(buffer)
                    if (n < 0) break
                    total += n
                    require(total <= MAX_FILE) { "Files must be at most 64 MiB" }
                    output.write(buffer, 0, n)
                  }
                }
              } ?: error("Couldn't read the shared file")
              descriptors.add(ParcelFileDescriptor.open(copy, ParcelFileDescriptor.MODE_READ_ONLY))
            }
            val subject = intent.getStringExtra(Intent.EXTRA_SUBJECT).orEmpty()
            val bytes =
              Wire.Writer()
                .u32(1)
                .text(text, MAX_TEXT)
                .text(subject, 4096)
                .apply {
                  names.forEach { text(it, 255) }
                }
                .bytes()
            metadata = Blob.create(bytes)
            val feature = IntegrationFeature.instance ?: error("Desktop bridge is unavailable")
            val sent =
              feature.connection.writeFiles(
                Wire.HANDOFF,
                arrayOf(metadata!!) + descriptors.map { it.fileDescriptor },
                {
                  u32(window)
                    .u8(action)
                    .u8(streams.size)
                    .u8(if (ask) 1 else 0)
                    .u8(0)
                    .u32(bytes.size)
                },
                { reply ->
                  try {
                    runOnUiThread {
                      if (reply.ok) finish()
                      else fail("Couldn't hand off to the desktop (${reply.status})")
                    }
                  } finally {
                    reply.closeFds()
                  }
                },
              )
            if (!sent) error("Desktop bridge is unavailable")
          } catch (e: Exception) {
            Log.w(TAG, "handoff", e)
            runOnUiThread { fail(e.message ?: "Couldn't share to the desktop") }
          } finally {
            Blob.close(metadata)
            descriptors.forEach { it.close() }
            copies.forEach { it.delete() }
          }
        },
        "desktop-handoff",
      )
      .start()
  }

  private fun displayName(uri: Uri): String {
    val queried =
      contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use {
        if (it.moveToFirst()) it.getString(0) else null
      }
    val name = queried ?: uri.lastPathSegment ?: "Shared file"
    val clean = name.replace('/', '_').replace('\\', '_').filter { !it.isISOControl() }
    return if (clean.isEmpty() || clean == "." || clean == "..") "Shared file"
    else String(Wire.truncateUtf8(clean, 255), Charsets.UTF_8)
  }

  private fun fail(message: String) {
    Toast.makeText(this, message, Toast.LENGTH_LONG).show()
    finish()
  }

  companion object {
    private const val TAG = "InnkeeperHandoff"
    private const val URI = 1
    private const val OPEN = 2
    private const val SAVE = 3
    private const val EMAIL = 4
    private const val COPY = 5
    private const val MAX_FILES = 3 // Four bridge fds: metadata plus three streams.
    private const val MAX_TEXT = 32 * 1024
    private const val MAX_FILE = 64L * 1024 * 1024
  }
}
