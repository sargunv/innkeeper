// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system

import android.app.Application
import android.util.Log
import dev.linuxapk.system.apps.AppMirrorFeature
import dev.linuxapk.system.ime.ImeFeature
import dev.linuxapk.system.input.InputBridge
import dev.linuxapk.system.input.InputFeature
import dev.linuxapk.system.integration.IntegrationFeature
import dev.linuxapk.system.wm.WindowManagerFeature

/**
 * The persistent process. ActivityManager starts it at boot (systemReady, before the user is
 * unlocked: the app is direct-boot aware) and restarts it if it dies. Each desktop-integration area
 * is a [Feature]: host input, then the window manager, which talks to the composer over the input
 * feature's socket, then desktop integration (settings, notifications, clipboard, links, media).
 */
class LinuxApkSystemApp : Application() {
  private val features = mutableListOf<Feature>()

  /** The window manager, once started (null if it failed to). */
  @Volatile
  var windows: WindowManagerFeature? = null
    private set

  @Volatile private var input: InputFeature? = null

  /** The composer connection (input stream and window-manager commands), shared by features. */
  val inputBridge: InputBridge?
    get() = input?.bridge

  override fun onCreate() {
    super.onCreate()
    Log.i(TAG, "starting")
    val input = InputFeature(this).also { this.input = it }
    val wm = WindowManagerFeature(this) { input.bridge }
    // Desktop integration over the host bridge (docs/bridge.md, "System service").
    val integration = IntegrationFeature(this)
    for (f in listOf<Feature>(input, wm, ImeFeature(this), integration, AppMirrorFeature(this))) {
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
