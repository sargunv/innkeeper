// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.probes

import android.app.Activity
import android.content.Intent
import android.os.Bundle
import android.util.Log

/** An app-originated file-picker handoff. It is not shipped. */
class DesktopProbe : Activity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    startActivityForResult(Intent(Intent.ACTION_OPEN_DOCUMENT).setType("text/plain"), 1)
  }

  @Deprecated("Diagnostic uses the framework activity result API")
  override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
    super.onActivityResult(requestCode, resultCode, data)
    val text =
      data?.data?.let { uri ->
        contentResolver.openInputStream(uri)?.bufferedReader()?.use { it.readText() }
      }
    Log.i("InnkeeperDesktopProbe", "picked: $text")
    finish()
  }
}
