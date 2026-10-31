// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.probes

import android.app.Activity
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.Process
import android.util.Log
import android.widget.TextView

/**
 * A configuration-change-naive activity for closing and recreating windows (tests/e2e storage, the
 * display teardown check): it handles no configuration changes, so moving its task between displays
 * relaunches it for the configuration change, and it finishes itself like the storage probe did
 * when the scan completed (InnkeeperStorageProbe pre-#139).
 *
 * Modes (via --es mode):
 * - default: finishes itself ~1.5 s after creation, as the old storage scan did.
 * - `finish`: finishes itself whenever it is recreated (self-kill on display move).
 * - `kill`: kills its own process whenever it is recreated.
 *
 * Manifest: no android:configChanges; the probes application is not game-category, so no
 * display-compat suppression applies (AppCompatDisplayCompatPolicy).
 */
class TeardownProbe : Activity() {
  private var relaunched = false

  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    relaunched = savedInstanceState != null
    when (intent.getStringExtra("mode")) {
      "finish" ->
        if (relaunched) {
          Log.i(TAG, "self-finishing on recreation (self-kill repro)")
          finish()
          return
        }
      "kill" ->
        if (relaunched) {
          Log.i(TAG, "killing own process on recreation (self-kill repro)")
          Process.killProcess(Process.myPid())
          return
        }
    }
    setContentView(TextView(this).apply { textSize = 24f })
    Log.i(TAG, "created, display=${display?.displayId} relaunched=$relaunched")
    if (relaunched) {
      Log.i(TAG, "self-finishing after recreation (self-kill repro)")
      finish()
      return
    }
    Handler(Looper.getMainLooper())
      .postDelayed(
        {
          Log.i(TAG, "self-finishing like the storage scan end (display=${display?.displayId})")
          finish()
        },
        FINISH_DELAY_MS,
      )
  }

  override fun onDestroy() {
    Log.i(TAG, "destroyed, display=${display?.displayId}")
    super.onDestroy()
  }

  companion object {
    const val TAG = "InnkeeperTeardownProbe"
    const val FINISH_DELAY_MS = 1_500L
  }
}
