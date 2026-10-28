// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.integration

import android.content.Context
import android.os.Handler
import android.os.HandlerThread
import android.util.Log
import dev.linuxapk.system.Feature
import dev.linuxapk.system.bridge.BridgeConnection
import dev.linuxapk.system.bridge.Wire
import dev.linuxapk.system.integration.location.HostLocation
import dev.linuxapk.system.integration.network.HostMetered
import dev.linuxapk.system.integration.power.PowerSaver
import java.io.FileDescriptor
import java.nio.ByteBuffer

/**
 * The host's state over the bridge's host state service (host/bridge/src/hoststate/proto.rs): the
 * power saver, the network's metering and location. (The battery goes to the Health HAL,
 * android/vendor/health, over a connection of its own.) Events are handled on the feature's thread.
 */
class HostStateFeature(private val context: Context) : Feature, BridgeConnection.Listener {
  override val name = "host state"

  private val thread = HandlerThread("linuxapk-hoststate")
  lateinit var handler: Handler
    private set

  val connection =
    BridgeConnection(BridgeConnection.SERVICE_HOSTSTATE, "Linux.apk System host state", this)
  private val saver = PowerSaver(context)
  private val metered = HostMetered()
  val location = HostLocation(context, this)

  override fun start() {
    thread.start()
    handler = Handler(thread.looper)
    instance = this
    handler.post { location.start() }
    connection.start()
  }

  override fun stop() {
    connection.stop()
    handler.post { location.stop() }
    instance = null
    thread.quitSafely()
  }

  // The current state follows START as events, so the resets go first.
  override fun onConnected() {
    handler.post {
      saver.reset()
      metered.reset()
    }
    connection.send(Wire.HS_START, onReply = ::onStarted) { u32(FEATURES) }
  }

  private fun onStarted(r: BridgeConnection.Reply) {
    val have = if (r.ok && r.body.remaining() >= 4) r.body.int else 0
    Log.i(TAG, "started (features 0x${have.toString(16)})")
    handler.post { location.onConnected(have and Wire.HS_F_LOCATION != 0) }
  }

  override fun onDisconnected() {
    handler.post { location.onDisconnected() }
  }

  override fun onEvent(type: Int, body: ByteBuffer, fds: Array<FileDescriptor>) {
    when (type) {
      Wire.EV_POWER_SAVER -> {
        val on = body.get().toInt() != 0
        handler.post { saver.apply(on) }
      }
      Wire.EV_NETWORK -> {
        body.get() // available
        val m = body.get().toInt() and 0xff
        handler.post { metered.apply(m) }
      }
      Wire.EV_LOCATION -> {
        val fix = HostLocation.Fix.decode(body) ?: return
        handler.post { location.onFix(fix) }
      }
      Wire.EV_LOCATION_STATUS -> {
        val s = body.get().toInt() and 0xff
        handler.post { location.onStatus(s) }
      }
      else -> Log.d(TAG, "event 0x${type.toString(16)} ignored")
    }
  }

  companion object {
    const val TAG = "LinuxApkHostState"
    private const val FEATURES = Wire.HS_F_POWER_SAVER or Wire.HS_F_NETWORK or Wire.HS_F_LOCATION

    /** The running feature, for the location provider service Android binds. */
    @Volatile var instance: HostStateFeature? = null
  }
}
