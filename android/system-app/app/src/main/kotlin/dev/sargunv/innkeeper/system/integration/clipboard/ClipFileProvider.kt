// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.integration.clipboard

import android.content.ContentProvider
import android.content.ContentValues
import android.content.Context
import android.database.Cursor
import android.database.MatrixCursor
import android.net.Uri
import android.os.ParcelFileDescriptor
import android.provider.OpenableColumns
import java.io.File
import java.io.FileNotFoundException
import java.util.UUID

/** Private, read-only cache. ClipboardService grants pasting apps access to the clip URI. */
class ClipFileProvider : ContentProvider() {
  override fun onCreate() = true

  private fun directory(uri: Uri): File {
    val id = uri.pathSegments.firstOrNull() ?: throw FileNotFoundException()
    if (!Regex("[0-9a-f-]{36}").matches(id)) throw FileNotFoundException()
    return File(context!!.cacheDir, "clip-files/$id")
  }

  override fun getType(uri: Uri): String? =
    directory(uri).resolve("mime").takeIf { it.isFile }?.readText()

  override fun openFile(uri: Uri, mode: String): ParcelFileDescriptor {
    if (mode != "r") throw SecurityException("read-only")
    return ParcelFileDescriptor.open(
      directory(uri).resolve("data"),
      ParcelFileDescriptor.MODE_READ_ONLY,
    )
  }

  override fun query(
    uri: Uri,
    projection: Array<out String>?,
    selection: String?,
    selectionArgs: Array<out String>?,
    sortOrder: String?,
  ): Cursor? {
    val d = directory(uri)
    val f = d.resolve("data")
    if (!f.isFile) return null
    val columns = projection ?: arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE)
    return MatrixCursor(columns).apply {
      addRow(
        columns
          .map<String, Any?> {
            when (it) {
              OpenableColumns.DISPLAY_NAME -> d.resolve("name").readText()
              OpenableColumns.SIZE -> f.length()
              else -> null
            }
          }
          .toTypedArray(),
      )
    }
  }

  override fun insert(uri: Uri, values: ContentValues?): Uri? = null

  override fun delete(uri: Uri, selection: String?, selectionArgs: Array<out String>?) = 0

  override fun update(
    uri: Uri,
    values: ContentValues?,
    selection: String?,
    selectionArgs: Array<out String>?,
  ) = 0

  companion object {
    const val AUTHORITY = "dev.sargunv.innkeeper.system.clipboard.files"

    @Synchronized
    fun store(context: Context, clip: ClipFiles): List<Uri> {
      val root = File(context.cacheDir, "clip-files").apply { mkdirs() }
      root.listFiles()?.forEach { it.deleteRecursively() }
      return clip.files.map { file ->
        val id = UUID.randomUUID().toString()
        val d = File(root, id).apply { mkdir() }
        d.resolve("data").writeBytes(file.bytes)
        d.resolve("name").writeText(file.name)
        d.resolve("mime").writeText(file.mime)
        Uri.Builder()
          .scheme("content")
          .authority(AUTHORITY)
          .appendPath(id)
          .appendPath(file.name)
          .build()
      }
    }
  }
}
