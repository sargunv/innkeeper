// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.integration.clipboard

import android.app.Activity
import android.content.ClipData
import android.content.ClipboardManager
import android.os.Bundle
import android.util.Log

/**
 * Diagnostic for the clipboard sync (docs/bridge.md, "Clipboard"). Shows nothing.
 *
 * am start -n dev.linuxapk.system/.integration.clipboard.ClipProbe --es text "copied in Android" am
 * start -n dev.linuxapk.system/.integration.clipboard.ClipProbe --ez image true # a test image am
 * start -n dev.linuxapk.system/.integration.clipboard.ClipProbe # logs the current clip
 *
 * Output goes to logcat, tag LinuxApkClipProbe.
 */
class ClipProbe : Activity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    val cm = getSystemService(ClipboardManager::class.java)
    val text = intent.getStringExtra("text")
    if (intent.getBooleanExtra("image", false)) {
      val bmp =
        android.graphics.Bitmap.createBitmap(64, 48, android.graphics.Bitmap.Config.ARGB_8888)
      bmp.eraseColor(android.graphics.Color.rgb(0x3d, 0xdc, 0x84))
      val uri = ClipImageProvider.store(this, bmp)
      cm.setPrimaryClip(ClipData.newUri(contentResolver, "probe image", uri))
      Log.i(TAG, "set image: $uri")
    } else if (text != null) {
      cm.setPrimaryClip(ClipData.newPlainText("probe", text))
      Log.i(TAG, "set: $text")
    } else {
      val clip = cm.primaryClip
      val item = clip?.takeIf { it.itemCount > 0 }?.getItemAt(0)
      Log.i(TAG, "clip: label=${clip?.description?.label} text=${item?.text} uri=${item?.uri}")
    }
    finish()
  }

  companion object {
    const val TAG = "LinuxApkClipProbe"
  }
}
