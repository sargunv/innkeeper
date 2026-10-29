// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.probes

import android.app.Activity
import android.os.Bundle
import android.util.Log
import android.widget.TextView

/**
 * A standard activity that handles none of the configuration changes a display move delivers.
 * Closing its window while Android 17's display-compat recovery can still relaunch onto that
 * display kills system_server; the e2e input group checks that it stays up.
 */
class DisplayCompatProbe : Activity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    setContentView(TextView(this).apply { text = "display compat probe" })
    Log.i(TAG, "created display=${display?.displayId}")
  }

  private companion object {
    const val TAG = "LinuxApkDisplayCompat"
  }
}
