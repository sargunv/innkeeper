// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.probes

import android.app.Activity
import android.content.ClipData
import android.content.ClipboardManager
import android.os.Bundle
import android.util.Log

/**
 * Diagnostic for the clipboard sync (host/bridge/src/system/clipboard.rs). Shows nothing; logs to
 * tag LinuxApkClipProbe.
 * - `am start -n dev.linuxapk.probes/.ClipProbe --es text "copied in Android"` copies text;
 * - `am start -n dev.linuxapk.probes/.ClipProbe --ez image true` copies a test image;
 * - `am start -n dev.linuxapk.probes/.ClipProbe` logs the current clip.
 */
class ClipProbe : Activity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    val cm = getSystemService(ClipboardManager::class.java)
    val text = intent.getStringExtra("text")
    if (intent.getBooleanExtra("file", false)) {
      val uri = ClipImageProbe.storeFile(this)
      cm.setPrimaryClip(ClipData.newUri(contentResolver, "probe file", uri))
      Log.i(TAG, "set file: $uri")
    } else if (intent.getBooleanExtra("image", false)) {
      val uri = ClipImageProbe.store(this)
      cm.setPrimaryClip(ClipData.newUri(contentResolver, "probe image", uri))
      Log.i(TAG, "set image: $uri")
    } else if (text != null) {
      cm.setPrimaryClip(ClipData.newPlainText("probe", text))
      Log.i(TAG, "set: $text")
    } else {
      val clip = cm.primaryClip
      val item = clip?.takeIf { it.itemCount > 0 }?.getItemAt(0)
      item?.uri?.let { uri ->
        try {
          val bytes = contentResolver.openInputStream(uri)?.use { it.readBytes() } ?: byteArrayOf()
          val hash =
            java.security.MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") {
              "%02x".format(it)
            }
          Log.i(
            TAG,
            "file: uri=$uri mime=${contentResolver.getType(uri)} bytes=${bytes.size} sha256=$hash",
          )
        } catch (e: Exception) {
          Log.e(TAG, "reading file: $e")
        }
      }
      Log.i(TAG, "clip: label=${clip?.description?.label} text=${item?.text} uri=${item?.uri}")
    }
    finish()
  }

  companion object {
    const val TAG = "LinuxApkClipProbe"
  }
}
