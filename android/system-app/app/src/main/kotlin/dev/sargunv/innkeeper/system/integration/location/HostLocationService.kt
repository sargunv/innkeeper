// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.integration.location

import android.app.Service
import android.content.Context
import android.content.Intent
import android.location.provider.LocationProviderBase
import android.location.provider.ProviderProperties
import android.location.provider.ProviderRequest
import android.os.Bundle
import android.os.IBinder
import android.util.Log
import dev.sargunv.innkeeper.system.integration.HostStateFeature

/**
 * Android's network location provider. system_server binds the highest `serviceVersion` among
 * services for `com.android.location.service.v3.NetworkLocationProvider`, since the image enables
 * the overlay with `config_enableNetworkLocationOverlay`. This one is version 0, so any other
 * provider outranks it. The fixes come from [HostLocation].
 */
class HostLocationService : Service() {
  private var provider: Provider? = null

  override fun onBind(intent: Intent): IBinder? {
    val p = provider ?: Provider(this).also { provider = it }
    HostStateFeature.instance?.let { f -> f.handler.post { f.location.onProviderBound(p) } }
    return p.getBinder()
  }

  class Provider(context: Context) :
    LocationProviderBase(
      context,
      HostLocation.TAG,
      ProviderProperties.Builder()
        .setPowerUsage(ProviderProperties.POWER_USAGE_LOW)
        .setAccuracy(ProviderProperties.ACCURACY_FINE)
        .build(),
    ) {
    override fun onSetRequest(request: ProviderRequest) {
      Log.d(HostLocation.TAG, "network request: active=${request.isActive}")
      // LocationRequest.QUALITY_HIGH_ACCURACY is 100.
      val precise = request.quality <= QUALITY_HIGH_ACCURACY
      HostStateFeature.instance?.let { f ->
        f.handler.post { f.location.onNetworkRequest(request.isActive, precise) }
      }
    }

    override fun onFlush(callback: OnFlushCompleteCallback) = callback.onFlushComplete()

    override fun onSendExtraCommand(command: String, extras: Bundle?) {}
  }

  private companion object {
    const val QUALITY_HIGH_ACCURACY = 100
  }
}
