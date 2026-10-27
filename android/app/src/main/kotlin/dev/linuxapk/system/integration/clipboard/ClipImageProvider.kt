// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.integration.clipboard

import android.content.ContentProvider
import android.content.ContentValues
import android.content.Context
import android.database.Cursor
import android.database.MatrixCursor
import android.graphics.Bitmap
import android.net.Uri
import android.os.ParcelFileDescriptor
import android.provider.OpenableColumns
import android.util.Log
import java.io.File
import java.io.FileNotFoundException

/**
 * Serves images copied on the desktop to Android apps that paste them:
 * `content://dev.linuxapk.system.clipboard/<n>.png`. Not exported; ClipboardService grants the
 * pasting app read access to the clip's URI. Only the latest image is kept.
 */
class ClipImageProvider : ContentProvider() {
  override fun onCreate() = true

  override fun getType(uri: Uri) = "image/png"

  override fun openFile(uri: Uri, mode: String): ParcelFileDescriptor {
    if (mode != "r") throw SecurityException("read-only")
    val f = file(context!!, uri) ?: throw FileNotFoundException(uri.toString())
    return ParcelFileDescriptor.open(f, ParcelFileDescriptor.MODE_READ_ONLY)
  }

  override fun query(
    uri: Uri,
    projection: Array<out String>?,
    selection: String?,
    selectionArgs: Array<out String>?,
    sortOrder: String?,
  ): Cursor? {
    val f = file(context!!, uri) ?: return null
    return MatrixCursor(arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE)).apply {
      addRow(arrayOf<Any>("desktop-clipboard.png", f.length()))
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
    const val AUTHORITY = "dev.linuxapk.system.clipboard"
    private var serial = 0

    private fun dir(c: Context) = File(c.cacheDir, "clip").apply { mkdirs() }

    private fun file(c: Context, uri: Uri): File? {
      val name = uri.lastPathSegment ?: return null
      if (!Regex("[0-9]+\\.png").matches(name)) return null
      return File(dir(c), name).takeIf { it.isFile }
    }

    /** Saves [bmp] as the current clipboard image; returns its URI. */
    @Synchronized
    fun store(c: Context, bmp: Bitmap): Uri? =
      try {
        val d = dir(c)
        d.listFiles()?.forEach { it.delete() }
        val name = "${++serial}.png"
        File(d, name).outputStream().use { bmp.compress(Bitmap.CompressFormat.PNG, 100, it) }
        Uri.parse("content://$AUTHORITY/$name")
      } catch (e: Exception) {
        Log.w(ClipboardSync.TAG, "storing the clipboard image: $e")
        null
      }
  }
}
