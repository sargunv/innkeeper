// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.probes

import android.content.ContentProvider
import android.content.ContentValues
import android.content.Context
import android.graphics.Bitmap
import android.graphics.Color
import android.net.Uri
import android.os.ParcelFileDescriptor
import java.io.File

/**
 * [ClipProbe]'s test image, a 64x48 green PNG: `content://dev.linuxapk.probes.clip/probe.png`. Not
 * exported; ClipboardService grants readers of the clip access to it.
 */
class ClipImageProbe : ContentProvider() {
  override fun onCreate() = true

  override fun getType(uri: Uri) =
    if (uri.lastPathSegment?.endsWith(".txt") == true) "text/plain" else "image/png"

  override fun openFile(uri: Uri, mode: String): ParcelFileDescriptor =
    ParcelFileDescriptor.open(
      if (uri.lastPathSegment?.endsWith(".txt") == true)
        File(context!!.cacheDir, uri.lastPathSegment!!)
      else file(context!!),
      ParcelFileDescriptor.MODE_READ_ONLY,
    )

  override fun query(
    uri: Uri,
    projection: Array<out String>?,
    selection: String?,
    selectionArgs: Array<out String>?,
    sortOrder: String?,
  ) =
    android.database.MatrixCursor(arrayOf(android.provider.OpenableColumns.DISPLAY_NAME)).apply {
      addRow(arrayOf(uri.lastPathSegment))
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
    private fun file(c: Context) = File(c.cacheDir, "probe.png")

    fun storeFile(c: Context, name: String = "probe.txt"): Uri {
      File(c.cacheDir, name).writeText("linuxapk-file-from-android $name\n")
      return Uri.parse("content://dev.linuxapk.probes.clip/$name")
    }

    /** Writes the test image; returns its URI. */
    fun store(c: Context): Uri {
      val bmp = Bitmap.createBitmap(64, 48, Bitmap.Config.ARGB_8888)
      bmp.eraseColor(Color.rgb(0x3d, 0xdc, 0x84))
      file(c).outputStream().use { bmp.compress(Bitmap.CompressFormat.PNG, 100, it) }
      return Uri.parse("content://dev.linuxapk.probes.clip/probe.png")
    }
  }
}
