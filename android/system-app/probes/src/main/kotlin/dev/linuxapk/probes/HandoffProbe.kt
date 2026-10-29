// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.probes

import android.app.Activity
import android.content.ContentProvider
import android.content.ContentValues
import android.content.Intent
import android.database.MatrixCursor
import android.net.Uri
import android.os.Bundle
import android.os.ParcelFileDescriptor
import android.provider.OpenableColumns
import android.widget.Button
import java.io.File
import java.util.zip.ZipEntry
import java.util.zip.ZipOutputStream

/** Ordinary VIEW and sharesheet intents with a read-granted ODT, never shipped. */
class HandoffProbe : Activity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    val uri = Uri.parse("content://dev.linuxapk.probes.handoff/shared.odt")
    val file = File(cacheDir, "shared.odt")
    ZipOutputStream(file.outputStream()).use { zip ->
      zip.putNextEntry(ZipEntry("mimetype"))
      zip.write("application/vnd.oasis.opendocument.text".toByteArray())
      zip.closeEntry()
    }
    fun send() {
      val share =
        Intent(Intent.ACTION_SEND)
          .setType("text/plain")
          .putExtra(Intent.EXTRA_TEXT, "Linux.apk shared text")
          .putExtra(Intent.EXTRA_SUBJECT, "Linux.apk handoff")
      if (intent.getBooleanExtra("file", false)) {
        share
          .setType("application/vnd.oasis.opendocument.text")
          .putExtra(Intent.EXTRA_STREAM, uri)
          .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        share.clipData = android.content.ClipData.newRawUri("Shared document", uri)
      }
      startActivity(Intent.createChooser(share, "Share"))
    }
    setContentView(
      Button(this).apply {
        text = "Share"
        setOnClickListener { send() }
      },
    )
    when (intent.getStringExtra("action")) {
      "view" ->
        startActivity(
          Intent(Intent.ACTION_VIEW)
            .setDataAndType(uri, "application/vnd.oasis.opendocument.text")
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION),
        )
      "share" -> send()
    }
  }

  class FileProvider : ContentProvider() {
    override fun onCreate() = true

    override fun getType(uri: Uri) = "application/vnd.oasis.opendocument.text"

    override fun openFile(uri: Uri, mode: String): ParcelFileDescriptor =
      ParcelFileDescriptor.open(
        File(context!!.cacheDir, "shared.odt"),
        ParcelFileDescriptor.MODE_READ_ONLY,
      )

    override fun query(
      uri: Uri,
      projection: Array<out String>?,
      selection: String?,
      selectionArgs: Array<out String>?,
      sortOrder: String?,
    ) =
      MatrixCursor(arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE)).apply {
        addRow(arrayOf<Any>("shared.odt", File(context!!.cacheDir, "shared.odt").length()))
      }

    override fun insert(uri: Uri, values: ContentValues?): Uri? = null

    override fun delete(uri: Uri, selection: String?, selectionArgs: Array<out String>?) = 0

    override fun update(
      uri: Uri,
      values: ContentValues?,
      selection: String?,
      selectionArgs: Array<out String>?,
    ) = 0
  }
}
