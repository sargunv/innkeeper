// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.probes

import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.util.Log
import android.widget.TextView

/** Manual sign-in proof with a local redirect server and a host verified by `pm set-app-links`. */
class CustomTabProbe : Activity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    if (intent.action == Intent.ACTION_VIEW) {
      Log.i("LinuxApkCustomTabProbe", "callback: ${intent.data}")
      setContentView(TextView(this).apply { text = "App Link returned: ${intent.data}" })
      return
    }
    setContentView(TextView(this).apply { text = "Opening sign-in probe" })
    if (savedInstanceState != null) return
    val view =
      Intent(Intent.ACTION_VIEW, Uri.parse(intent.getStringExtra("url")))
        .addCategory(Intent.CATEGORY_BROWSABLE)
    if (!intent.getBooleanExtra("plain", false)) {
      // AndroidX CustomTabsIntent includes SESSION even when no service session was bound.
      view.putExtras(Bundle().apply { putBinder("android.support.customtabs.extra.SESSION", null) })
    }
    startActivity(view)
  }
}
