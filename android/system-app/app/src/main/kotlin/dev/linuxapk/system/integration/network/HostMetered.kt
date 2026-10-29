// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.integration.network

import android.util.Log
import dev.linuxapk.system.net.HostNetworkFeature

/**
 * The host connection's metering (NetworkMonitor portal) on Android's default network, so apps that
 * honour "unmetered only" (F-Droid, AntennaPod) see the host's state.
 *
 * The host network agent ([HostNetworkFeature]) owns the capabilities.
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
      Log.i(TAG, "host network metered=$m (host network agent is not running)")
      return
    }
    // Unknown reads as unmetered, as Android's own default for wired networks.
    net.setMetered(m == 2)
  }

  private companion object {
    const val TAG = "LinuxApkHostState"
  }
}
