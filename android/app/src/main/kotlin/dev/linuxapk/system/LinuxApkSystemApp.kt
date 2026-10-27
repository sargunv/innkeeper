// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system

import android.app.Application
import android.util.Log
import dev.linuxapk.system.input.InputFeature
import dev.linuxapk.system.wm.WindowManagerFeature

/**
 * The persistent process. ActivityManager starts it at boot (systemReady, before the user is
 * unlocked: the app is direct-boot aware) and restarts it if it dies. Each desktop-integration area
 * is a [Feature]: host input, then the window manager, which talks to the composer over the input
 * feature's socket. M3 adds dark mode, notifications, file pickers and clipboard here.
 */
class LinuxApkSystemApp : Application() {
  private val features = mutableListOf<Feature>()

  /** The window manager, once started (null if it failed to). */
  @Volatile
  var windows: WindowManagerFeature? = null
    private set

  override fun onCreate() {
    super.onCreate()
    Log.i(TAG, "starting")
    val input = InputFeature(this)
    val wm = WindowManagerFeature(this) { input.bridge }
    for (f in listOf<Feature>(input, wm)) {
      try {
        f.start()
        features += f
        Log.i(TAG, "started ${f.name}")
      } catch (e: Exception) {
        Log.e(TAG, "feature ${f.name} failed to start", e)
      }
    }
    if (wm in features) windows = wm
  }

  companion object {
    const val TAG = "LinuxApkSystem"
  }
}
