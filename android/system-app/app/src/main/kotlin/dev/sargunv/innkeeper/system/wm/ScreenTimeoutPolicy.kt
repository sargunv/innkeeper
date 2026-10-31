// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.wm

import android.os.IPowerManager
import android.os.IScreenTimeoutPolicyListener
import android.os.ServiceManager

/** Android 17's per-display policy includes screen wake locks and FLAG_KEEP_SCREEN_ON. */
class ScreenTimeoutPolicy(display: Int, changed: (Boolean) -> Unit) : AutoCloseable {
  private val display = display
  private val power = IPowerManager.Stub.asInterface(ServiceManager.getService("power"))
  private val listener =
    object : IScreenTimeoutPolicyListener.Stub() {
      override fun onScreenTimeoutPolicyChanged(policy: Int) = changed(policy == KEEP_DISPLAY_ON)
    }

  init {
    power.addScreenTimeoutPolicyListener(display, listener)
  }

  override fun close() {
    power.removeScreenTimeoutPolicyListener(display, listener)
  }

  companion object {
    private const val KEEP_DISPLAY_ON = 1
  }
}
