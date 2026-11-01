// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.integration.power

import android.content.Context
import android.os.IPowerManager
import android.os.PowerManager
import android.os.ServiceManager
import android.util.Log

/**
 * The desktop's PowerProfileMonitor portal as Android's battery saver, which apps read from
 * `PowerManager.isPowerSaveMode` and its broadcast (AntennaPod pauses downloads). The desktop's
 * value wins whenever it is sent, which is at each connect and on each change.
 */
class PowerSaver(context: Context) {
  private val pm = context.getSystemService(PowerManager::class.java)
  private val power by lazy { IPowerManager.Stub.asInterface(ServiceManager.getService("power")) }
  private var last: Boolean? = null

  fun reset() {
    last = null
  }

  fun apply(on: Boolean) {
    if (last == on) return
    last = on
    if (pm.isPowerSaveMode == on) return
    try {
      // DEVICE_POWER.
      val ok = power.setPowerSaveModeEnabled(on)
      Log.i(TAG, "battery saver -> $on${if (ok) "" else " (refused)"}")
    } catch (e: Exception) {
      Log.w(TAG, "battery saver: $e")
    }
  }

  private companion object {
    const val TAG = "InnkeeperHostState"
  }
}
