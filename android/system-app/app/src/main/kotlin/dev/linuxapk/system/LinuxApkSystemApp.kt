// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system

import android.app.Application
import android.util.Log
import dev.linuxapk.system.a11y.A11yFeature
import dev.linuxapk.system.apps.AppMirrorFeature
import dev.linuxapk.system.ime.ImeFeature
import dev.linuxapk.system.input.HostInputClient
import dev.linuxapk.system.integration.IntegrationFeature
import dev.linuxapk.system.net.HostNetworkFeature
import dev.linuxapk.system.wm.WindowManagerFeature

/**
 * The persistent process. ActivityManager starts it at boot (systemReady, before the user is
 * unlocked: the app is direct-boot aware) and restarts it if it dies. Each desktop-integration area
 * is a [Feature]: host input, then the window manager and the IME, which share its connection to
 * the bridge's input service, then desktop integration (settings, notifications, clipboard, links,
 * media), the app mirror, in Flatpak mode the host network, and accessibility.
 */
class LinuxApkSystemApp : Application() {
  private val features = mutableListOf<Feature>()

  /** The bridge's input service: window requests, host input, the host's input methods. */
  val input = HostInputClient()

  /** Android apps on the desktop's AT-SPI bus; its accessibility service finds it here. */
  val a11y = A11yFeature(this)

  /** The window manager, once started (null if it failed to). */
  @Volatile
  var windows: WindowManagerFeature? = null
    private set

  override fun onCreate() {
    super.onCreate()
    Log.i(TAG, "starting")
    val wm = WindowManagerFeature(this, input)
    for (f in
      listOf(
        input,
        wm,
        ImeFeature(this),
        IntegrationFeature(this),
        AppMirrorFeature(this),
        HostNetworkFeature(this),
        a11y,
      )) {
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
