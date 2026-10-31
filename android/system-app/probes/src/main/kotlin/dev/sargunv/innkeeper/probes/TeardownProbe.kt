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
 * An activity that handles no configuration changes, for the display teardown check: moving its
 * task between displays relaunches it, and it finishes itself while that happens.
 *
 * Modes (via --es mode):
 * - default: finishes itself ~1.5 s after creation.
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
