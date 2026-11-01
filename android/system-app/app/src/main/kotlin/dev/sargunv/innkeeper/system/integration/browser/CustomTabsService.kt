// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.integration.browser

import android.app.Service
import android.content.Intent
import android.os.Binder
import android.os.IBinder
import android.os.Parcel

/**
 * A Custom Tabs provider, so apps (AppAuth sign-ins, "open in browser" helpers) find a browser that
 * supports Custom Tabs. It accepts sessions and warmup and does nothing else. The tab itself is a
 * `VIEW` intent to [BrowserActivity], which chooses the desktop or an Android browser. It
 * implements `android.support.customtabs.ICustomTabsService` by hand, without AndroidX.
 */
class CustomTabsService : Service() {
  private val binder =
    object : Binder() {
      override fun onTransact(code: Int, data: Parcel, reply: Parcel?, flags: Int): Boolean {
        if (code == INTERFACE_TRANSACTION) {
          reply?.writeString(DESCRIPTOR)
          return true
        }
        if (code !in FIRST_CALL_TRANSACTION..LAST_CALL_TRANSACTION) {
          return super.onTransact(code, data, reply, flags)
        }
        data.enforceInterface(DESCRIPTOR)
        reply?.writeNoException()
        when (code) {
          WARMUP,
          NEW_SESSION,
          MAY_LAUNCH_URL,
          NEW_SESSION_WITH_EXTRAS -> reply?.writeInt(1)
          EXTRA_COMMAND -> reply?.writeInt(0) // null Bundle
          POST_MESSAGE -> reply?.writeInt(-3) // RESULT_FAILURE_MESSAGING_ERROR
          else -> reply?.writeInt(0) // false
        }
        return true
      }
    }

  override fun onBind(intent: Intent?): IBinder = binder

  companion object {
    const val DESCRIPTOR = "android.support.customtabs.ICustomTabsService"
    // Transaction codes from androidx.browser's ICustomTabsService.aidl.
    private const val WARMUP = 1
    private const val NEW_SESSION = 2
    private const val MAY_LAUNCH_URL = 3
    private const val EXTRA_COMMAND = 4
    private const val POST_MESSAGE = 7
    private const val NEW_SESSION_WITH_EXTRAS = 9
  }
}
