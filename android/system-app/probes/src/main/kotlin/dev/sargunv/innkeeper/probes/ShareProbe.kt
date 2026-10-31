// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.probes

import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.util.Log
import android.widget.TextView
import java.security.MessageDigest

/** Host share receiver: reads the real streams, checks ClipData and refuses writes. */
class ShareProbe : Activity() {
  override fun onCreate(state: Bundle?) {
    super.onCreate(state)
    show(intent)
  }

  override fun onNewIntent(intent: Intent) {
    super.onNewIntent(intent)
    show(intent)
  }

  private fun show(intent: Intent) {
    val uris =
      when (intent.action) {
        Intent.ACTION_SEND ->
          listOfNotNull(intent.getParcelableExtra(Intent.EXTRA_STREAM, Uri::class.java))
        Intent.ACTION_SEND_MULTIPLE ->
          intent.getParcelableArrayListExtra(Intent.EXTRA_STREAM, Uri::class.java).orEmpty()
        else -> error("Expected SEND or SEND_MULTIPLE")
      }
    val clip = intent.clipData!!
    check(uris.isNotEmpty() && uris.size == clip.itemCount)
    check(uris.indices.all { uris[it] == clip.getItemAt(it).uri })
    check(intent.flags and Intent.FLAG_GRANT_READ_URI_PERMISSION != 0)
    check(intent.flags and Intent.FLAG_GRANT_WRITE_URI_PERMISSION == 0)
    val results = uris.mapIndexed { index, uri ->
      check(uri.scheme == "content")
      val bytes = contentResolver.openInputStream(uri)!!.use { it.readBytes() }
      val sha =
        MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }
      val writable =
        try {
          contentResolver.openFileDescriptor(uri, "w")!!.close()
          true
        } catch (_: Exception) {
          false
        }
      check(!writable)
      "item=$index bytes=${bytes.size} sha256=$sha uri=$uri write=$writable"
    }
    val report =
      "action=${intent.action} type=${intent.type} count=${uris.size} read=true clip=true\n${results.joinToString("\n")}"
    Log.i("InnkeeperShareProbe", report)
    setContentView(
      TextView(this).apply {
        text = report
        textSize = 18f
      },
    )
  }
}
