// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.integration.location

import android.app.AppOpsManager
import android.content.Context
import android.location.Location
import android.location.LocationManager
import android.location.provider.ProviderProperties
import android.location.provider.ProviderRequest
import android.os.Process
import android.os.SystemClock
import android.util.Log
import dev.sargunv.innkeeper.system.bridge.Wire
import dev.sargunv.innkeeper.system.integration.HostStateFeature
import java.nio.ByteBuffer

/**
 * The desktop's location as Android's: one host source (the Location portal, through the bridge)
 * feeds the network provider ([HostLocationService]), and through a test provider the gps one;
 * Android's FusedLocation combines them as usual.
 *
 * Consent (#19): Android's runtime permission is the primary consent, so the bridge asks the
 * desktop only once an app Android allowed asks for a location (a request on either provider). If
 * the user denies the desktop's dialog, both providers read as disabled for the rest of the
 * session, while the app's Android permission stays granted.
 *
 * The session has no GNSS HAL, so gps is a test provider (its fixes carry `isMock`), and its
 * requests are seen through `addProviderRequestChangedListener`. A GNSS AIDL HAL would replace it.
 * Runs on [HostStateFeature]'s thread.
 */
class HostLocation(private val context: Context, private val feature: HostStateFeature) {
  /** One fix, as `LOCATION` carries it. */
  data class Fix(
    val latitude: Double,
    val longitude: Double,
    val altitude: Double,
    val accuracy: Float,
    val speed: Float,
    val bearing: Float,
    val timeMs: Long,
  ) {
    companion object {
      fun decode(r: ByteBuffer): Fix? {
        if (r.remaining() < 44) return null
        val f =
          Fix(
            java.lang.Double.longBitsToDouble(r.long),
            java.lang.Double.longBitsToDouble(r.long),
            java.lang.Double.longBitsToDouble(r.long),
            r.float,
            r.float,
            r.float,
            r.long,
          )
        val ok = f.latitude in -90.0..90.0 && f.longitude in -180.0..180.0 && f.accuracy.isFinite()
        return if (ok) f else null
      }
    }
  }

  private val lm = context.getSystemService(LocationManager::class.java)
  /** The network provider, once Android bound it. */
  @Volatile
  var provider: HostLocationService.Provider? = null
    private set

  private var connected = false
  private var available = false
  private var networkActive = false
  private var networkPrecise = false
  private var gpsActive = false
  private var gps = false
  /** LOCATION_START sent and not answered by a failure. */
  private var started = false
  private var startedPrecise = false
  /** The desktop said no (or has location off): providers off for this bridge. */
  private var refused = false

  private val requests = ProviderRequest.ChangedListener { name, request ->
    if (name == LocationManager.GPS_PROVIDER) {
      feature.handler.post {
        gpsActive = request.isActive
        update()
      }
    }
  }

  fun start() {
    gps = addGpsProvider()
    try {
      // LocationManager.addProviderRequestChangedListener, a system API (LOCATION_HARDWARE).
      LocationManager::class
        .java
        .getMethod(
          "addProviderRequestChangedListener",
          java.util.concurrent.Executor::class.java,
          ProviderRequest.ChangedListener::class.java,
        )
        .invoke(lm, context.mainExecutor, requests)
    } catch (e: Exception) {
      Log.w(TAG, "gps requests not visible: $e")
    }
  }

  /**
   * The gps provider, a test provider fed with the same fixes. Test providers need the mock
   * location app op, which this platform-signed app grants itself.
   */
  private fun addGpsProvider(): Boolean =
    try {
      AppOpsManager::class
        .java
        .getMethod(
          "setMode",
          String::class.java,
          Int::class.javaPrimitiveType,
          String::class.java,
          Int::class.javaPrimitiveType,
        )
        .invoke(
          context.getSystemService(AppOpsManager::class.java),
          AppOpsManager.OPSTR_MOCK_LOCATION,
          Process.myUid(),
          context.packageName,
          AppOpsManager.MODE_ALLOWED,
        )
      val props =
        ProviderProperties.Builder()
          .setHasSatelliteRequirement(true)
          .setHasAltitudeSupport(true)
          .setHasSpeedSupport(true)
          .setHasBearingSupport(true)
          .setPowerUsage(ProviderProperties.POWER_USAGE_HIGH)
          .setAccuracy(ProviderProperties.ACCURACY_FINE)
          .build()
      lm.addTestProvider(LocationManager.GPS_PROVIDER, props)
      lm.setTestProviderEnabled(LocationManager.GPS_PROVIDER, true)
      Log.i(TAG, "gps provider up (a test provider fed by the host)")
      true
    } catch (e: Exception) {
      Log.w(TAG, "no gps provider: $e")
      false
    }

  /** Android bound the network provider ([HostLocationService]). */
  fun onProviderBound(p: HostLocationService.Provider) {
    provider = p
    p.setAllowed(!refused)
  }

  /** The network provider's request changed. */
  fun onNetworkRequest(active: Boolean, precise: Boolean) {
    networkActive = active
    networkPrecise = precise
    update()
  }

  fun onConnected(hasLocation: Boolean) {
    connected = true
    available = hasLocation
    started = false
    update()
  }

  fun onDisconnected() {
    connected = false
    started = false
  }

  private val sync = Runnable { sync() }

  /**
   * Android's requests come in bursts (an app asking several providers, or re-registering), and
   * each new desktop session can put a dialog up, so changes settle before reaching the bridge: a
   * start after [START_DELAY_MS], a stop after [STOP_DELAY_MS].
   */
  private fun update() {
    feature.handler.removeCallbacks(sync)
    val delay = if (networkActive || gpsActive) START_DELAY_MS else STOP_DELAY_MS
    feature.handler.postDelayed(sync, delay)
  }

  /** Starts or stops the desktop's location as Android's requests come and go. */
  private fun sync() {
    val active = networkActive || gpsActive
    if (!connected || !available || refused) return
    val precise = gpsActive || networkPrecise
    // A start again when precision rises: the bridge reopens its session at that accuracy.
    if (active && (!started || (precise && !startedPrecise))) {
      started = true
      startedPrecise = precise
      Log.i(TAG, "location requested (precise $precise): asking the desktop")
      feature.connection.send(Wire.HS_LOCATION_START) { u8(if (precise) 1 else 0).u8(0).u16(0) }
    } else if (!active && started) {
      started = false
      Log.i(TAG, "no location requests: stopping")
      feature.connection.send(Wire.HS_LOCATION_STOP)
    }
  }

  fun onStatus(s: Int) {
    when (s) {
      STATUS_RUNNING -> Log.i(TAG, "the desktop shares its location")
      STATUS_DENIED,
      STATUS_DISABLED -> {
        Log.i(TAG, "the desktop refused location (${if (s == STATUS_DENIED) "denied" else "off"})")
        started = false
        refused = true
        setEnabled(false)
      }
      else -> {
        // Unavailable: try again on the next request change.
        Log.i(TAG, "desktop location unavailable")
        started = false
      }
    }
  }

  fun onFix(f: Fix) {
    if (refused) return
    provider?.reportLocation(location(LocationManager.NETWORK_PROVIDER, f))
    if (gps) {
      try {
        lm.setTestProviderLocation(
          LocationManager.GPS_PROVIDER,
          location(LocationManager.GPS_PROVIDER, f),
        )
      } catch (e: Exception) {
        Log.w(TAG, "gps fix: $e")
      }
    }
  }

  private fun setEnabled(on: Boolean) {
    provider?.setAllowed(on)
    if (gps) {
      try {
        lm.setTestProviderEnabled(LocationManager.GPS_PROVIDER, on)
      } catch (e: Exception) {
        Log.w(TAG, "gps enabled=$on: $e")
      }
    }
  }

  private fun location(name: String, f: Fix) =
    Location(name).apply {
      latitude = f.latitude
      longitude = f.longitude
      if (!f.altitude.isNaN()) altitude = f.altitude
      accuracy = if (f.accuracy >= 0f) f.accuracy else DEFAULT_ACCURACY
      if (f.speed >= 0f) speed = f.speed
      if (f.bearing >= 0f) bearing = f.bearing
      // The fix's own time; elapsed time is now, as for any freshly delivered fix.
      time = if (f.timeMs > 0) f.timeMs else System.currentTimeMillis()
      elapsedRealtimeNanos = SystemClock.elapsedRealtimeNanos()
    }

  companion object {
    const val TAG = "InnkeeperLocation"
    private const val STATUS_RUNNING = 1
    private const val STATUS_DENIED = 2
    private const val STATUS_DISABLED = 3
    private const val START_DELAY_MS = 300L
    private const val STOP_DELAY_MS = 3000L
    /** Meters, for a fix without one (Location requires an accuracy). */
    private const val DEFAULT_ACCURACY = 1000f
  }
}
