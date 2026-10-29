// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.probes

import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.util.Log

/** App-originated browser and file-picker handoffs, never shipped. */
class DesktopProbe : Activity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    if (intent.getStringExtra("action") == "link") {
      startActivity(Intent(Intent.ACTION_VIEW, Uri.parse("https://example.com/linuxapk-e2e")))
      finish()
    } else {
      startActivityForResult(Intent(Intent.ACTION_OPEN_DOCUMENT).setType("text/plain"), 1)
    }
  }

  @Deprecated("Diagnostic uses the framework activity result API")
  override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
    super.onActivityResult(requestCode, resultCode, data)
    val text =
      data?.data?.let { uri ->
        contentResolver.openInputStream(uri)?.bufferedReader()?.use { it.readText() }
      }
    Log.i("LinuxApkDesktopProbe", "picked: $text")
    finish()
  }
}
