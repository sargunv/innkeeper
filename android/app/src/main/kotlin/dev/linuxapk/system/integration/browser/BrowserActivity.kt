// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.integration.browser

import android.app.Activity
import android.content.Intent
import android.os.Bundle
import android.util.Log
import android.widget.Toast
import dev.linuxapk.system.integration.IntegrationFeature

/**
 * The fallback browser: `VIEW` http(s) links that no Android app handles (its intent filter has a
 * low priority, so real browsers and verified app links win), and Custom Tabs launches, open in the
 * host browser. It shows nothing and finishes at once.
 */
class BrowserActivity : Activity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    handle(intent)
    finish()
  }

  private fun handle(intent: Intent?) {
    val uri = intent?.data ?: return
    val ok = IntegrationFeature.instance?.urls?.open(uri.toString()) == true
    if (!ok) {
      Log.w(UrlOpener.TAG, "could not open $uri on the desktop")
      Toast.makeText(this, "Couldn't open the link on the desktop", Toast.LENGTH_SHORT).show()
    }
  }
}
