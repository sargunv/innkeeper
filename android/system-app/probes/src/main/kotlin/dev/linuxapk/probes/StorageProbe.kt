// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.probes

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.graphics.Bitmap
import android.media.MediaScannerConnection
import android.os.Environment
import android.provider.MediaStore
import android.util.Log
import java.io.File

/** Shared storage state, app directories, and a real MediaStore scan/read. Never shipped. */
class StorageProbe : BroadcastReceiver() {
  // No window: moving an Activity to an app display recreates it, starts another
  // scan and mistakes its intentional finish for Android's display self-kill recovery.
  override fun onReceive(context: Context, intent: Intent) {
    val pending = goAsync()
    val state = Environment.getExternalStorageState()
    check(state == Environment.MEDIA_MOUNTED) { "external storage state=$state" }
    val appDir = checkNotNull(context.getExternalFilesDir(null))
    val privateFile = File(appDir, "storage-probe.txt")
    privateFile.writeText("external app directory")
    check(privateFile.readText() == "external app directory")
    privateFile.delete()

    val pictures = Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_PICTURES)
    check(pictures.mkdirs() || pictures.isDirectory)
    val file = File(pictures, "linuxapk-storage-probe.png")
    val bitmap = Bitmap.createBitmap(2, 2, Bitmap.Config.ARGB_8888)
    file.outputStream().use { check(bitmap.compress(Bitmap.CompressFormat.PNG, 100, it)) }
    bitmap.recycle()
    MediaScannerConnection.scanFile(context, arrayOf(file.path), arrayOf("image/png")) { _, uri ->
      checkNotNull(uri) { "scan returned no MediaStore URI" }
      context.contentResolver
        .query(uri, arrayOf(MediaStore.Images.Media.DISPLAY_NAME), null, null, null)
        ?.use { cursor ->
          check(cursor.moveToFirst() && cursor.getString(0) == file.name)
        } ?: error("MediaStore query failed")
      context.contentResolver.openInputStream(uri)?.use {
        check(it.readBytes().contentEquals(file.readBytes()))
      } ?: error("MediaStore read failed")
      check(context.contentResolver.delete(uri, null, null) == 1)
      check(!file.exists())
      Log.i(
        "LinuxApkStorageProbe",
        "state=$state app-dir=ok MediaStore=ok round=${intent.getStringExtra("round")}",
      )
      pending.finish()
    }
  }
}
