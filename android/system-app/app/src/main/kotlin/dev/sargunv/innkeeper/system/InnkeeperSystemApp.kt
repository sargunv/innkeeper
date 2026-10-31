// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system

import android.app.Application
import android.util.Log
import dev.sargunv.innkeeper.system.a11y.A11yFeature
import dev.sargunv.innkeeper.system.alarms.AlarmTickFeature
import dev.sargunv.innkeeper.system.apps.AppMirrorFeature
import dev.sargunv.innkeeper.system.apps.AppSettings
import dev.sargunv.innkeeper.system.ime.ImeFeature
import dev.sargunv.innkeeper.system.input.HostInputClient
import dev.sargunv.innkeeper.system.integration.HostStateFeature
import dev.sargunv.innkeeper.system.integration.IntegrationFeature
import dev.sargunv.innkeeper.system.integration.network.HostNetworkFeature
import dev.sargunv.innkeeper.system.wm.WindowManagerFeature

/**
 * The persistent process. ActivityManager starts it at boot (systemReady, before the user is
 * unlocked: the app is direct-boot aware) and restarts it if it dies. Each desktop-integration area
 * is a [Feature]: host input, then the window manager and the IME, which share its connection to
 * the bridge's input service, then desktop integration (settings, notifications, clipboard, links,
 * media), the app mirror, the host network, the host's state (power saver, metering, location),
 * accessibility, and a tick that delivers due inexact alarms.
 */
class InnkeeperSystemApp : Application() {
  private val features = mutableListOf<Feature>()

  /** The bridge's input service: window requests, host input, the host's input methods. */
  val appSettings by lazy { AppSettings(this) }

  val input = HostInputClient(this)

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
    val network = HostNetworkFeature(this)
    for (f in
      listOf(
        input,
        wm,
        ImeFeature(this),
        IntegrationFeature(this, wm),
        AppMirrorFeature(this, appSettings),
        network,
        HostStateFeature(this, network),
        a11y,
        AlarmTickFeature(this),
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
    const val TAG = "InnkeeperSystem"
  }
}
