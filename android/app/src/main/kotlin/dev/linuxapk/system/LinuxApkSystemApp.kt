// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system

import android.app.Application
import android.util.Log
import dev.linuxapk.system.input.InputFeature

/**
 * The persistent process. ActivityManager starts it at boot (systemReady, before the user is
 * unlocked: the app is direct-boot aware) and restarts it if it dies. Each desktop-integration area
 * is a [Feature]; input is the first. M3 adds dark mode, notifications, file pickers and clipboard
 * here.
 */
class LinuxApkSystemApp : Application() {
  private val features = mutableListOf<Feature>()

  override fun onCreate() {
    super.onCreate()
    Log.i(TAG, "starting")
    for (f in listOf<Feature>(InputFeature(this))) {
      try {
        f.start()
        features += f
        Log.i(TAG, "started ${f.name}")
      } catch (e: Exception) {
        Log.e(TAG, "feature ${f.name} failed to start", e)
      }
    }
  }

  companion object {
    const val TAG = "LinuxApkSystem"
  }
}
