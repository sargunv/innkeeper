// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.probes

import android.app.Activity
import android.content.Intent
import android.os.Bundle
import android.util.Log
import android.widget.TextView

/** Open With receiver that checks the supplied URI, MIME type, flags and file contents. */
class FileProbe : Activity() {
  override fun onCreate(state: Bundle?) {
    super.onCreate(state)
    show(intent)
  }

  override fun onNewIntent(intent: Intent) {
    super.onNewIntent(intent)
    show(intent)
  }

  private fun show(intent: Intent) {
    val text =
      intent.data?.let { uri ->
        contentResolver.openInputStream(uri)!!.bufferedReader().use { it.readText() }
      } ?: "Open a Markdown file from the desktop"
    Log.i(
      "InnkeeperFileProbe",
      "uri=${intent.data} type=${intent.type} read=${intent.flags and Intent.FLAG_GRANT_READ_URI_PERMISSION != 0} content=$text",
    )
    setContentView(
      TextView(this).apply {
        this.text = text
        textSize = 24f
      },
    )
  }
}
