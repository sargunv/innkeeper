// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.integration.network

import android.util.Log
import dev.linuxapk.system.net.HostNetworkFeature

/**
 * The host connection's metering (NetworkMonitor portal) on Android's default network, so apps that
 * honour "unmetered only" (F-Droid, AntennaPod) see the host's state.
 *
 * Only a network agent we own can change its capabilities: Flatpak mode's host network
 * ([HostNetworkFeature]). In host mode the network is EthernetService's, whose capabilities can
 * only be changed on automotive devices (`EthernetManager.updateConfiguration`), so there the
 * host's metering is logged and not applied.
 */
class HostMetered {
  private var last = -1

  fun reset() {
    last = -1
  }

  /** [m]: 0 unknown, 1 not metered, 2 metered. */
  fun apply(m: Int) {
    if (m == last) return
    last = m
    val net = HostNetworkFeature.instance
    if (net == null || !net.running) {
      Log.i(TAG, "host network metered=$m (not applied: Android's network is EthernetService's)")
      return
    }
    // Unknown reads as unmetered, as Android's own default for wired networks.
    net.setMetered(m == 2)
  }

  private companion object {
    const val TAG = "LinuxApkHostState"
  }
}
