// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.probes

import android.app.Activity
import android.location.Location
import android.location.LocationListener
import android.location.LocationManager
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.util.Log

/**
 * Diagnostic for host location (host/bridge/src/hoststate/location.rs): an app that asks for
 * locations, as a map does. Logs to tag LinuxApkLocationProbe whether each provider is enabled,
 * then every fix (`fix from=<provider asked> provider=<fix's provider>`: fused passes on its
 * sources' fixes), for `--ei seconds N` (default 20), then finishes. Install with `-g` for the
 * location permissions.
 * - `am start -n dev.linuxapk.probes/.LocationProbe [--es providers network,gps,fused]`
 */
class LocationProbe : Activity() {
  private lateinit var lm: LocationManager
  private val listeners = mutableListOf<LocationListener>()

  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    lm = getSystemService(LocationManager::class.java)
    val providers = (intent.getStringExtra("providers") ?: "network,gps,fused").split(',')
    Log.i(TAG, "location enabled=${lm.isLocationEnabled} providers=${lm.allProviders}")
    for (p in providers) {
      val on = lm.allProviders.contains(p) && lm.isProviderEnabled(p)
      Log.i(TAG, "provider $p enabled=$on")
      if (!lm.allProviders.contains(p)) continue
      try {
        val l = Listener(p).also { listeners += it }
        lm.requestLocationUpdates(p, 1000L, 0f, l, Looper.getMainLooper())
      } catch (e: Exception) {
        Log.w(TAG, "request $p: $e")
      }
    }
    Handler(Looper.getMainLooper())
      .postDelayed({ finish() }, intent.getIntExtra("seconds", 20) * 1000L)
  }

  private class Listener(val from: String) : LocationListener {
    override fun onLocationChanged(l: Location) {
      Log.i(
        TAG,
        "fix from=$from provider=${l.provider} lat=${l.latitude} lon=${l.longitude} " +
          "acc=${l.accuracy} mock=${l.isMock}",
      )
    }

    override fun onProviderDisabled(provider: String) {
      Log.i(TAG, "provider $provider disabled")
    }

    override fun onProviderEnabled(provider: String) {
      Log.i(TAG, "provider $provider enabled")
    }
  }

  override fun onDestroy() {
    for (l in listeners) lm.removeUpdates(l)
    super.onDestroy()
  }

  companion object {
    const val TAG = "LinuxApkLocationProbe"
  }
}
