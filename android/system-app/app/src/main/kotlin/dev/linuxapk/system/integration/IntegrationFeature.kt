// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.integration

import android.content.Context
import android.os.Handler
import android.os.HandlerThread
import android.util.Log
import dev.linuxapk.system.Feature
import dev.linuxapk.system.bridge.BridgeConnection
import dev.linuxapk.system.integration.browser.BrowserRole
import dev.linuxapk.system.integration.browser.UrlOpener
import dev.linuxapk.system.integration.clipboard.ClipboardSync
import dev.linuxapk.system.integration.media.MediaSync
import dev.linuxapk.system.integration.notifications.Notifications
import dev.linuxapk.system.integration.settings.DesktopSettings
import dev.linuxapk.system.integration.settings.HostSettings
import java.io.FileDescriptor
import java.nio.ByteBuffer

/**
 * Desktop integration over the host bridge's system service (docs/bridge.md, "System service"):
 * desktop settings into Android, notifications and media sessions out, the clipboard both ways, and
 * web links to the host browser. Events are handled on the feature's own thread.
 */
class IntegrationFeature(private val context: Context) : Feature, BridgeConnection.Listener {
  override val name = "integration"

  private val thread = HandlerThread("linuxapk-integ")
  lateinit var handler: Handler
    private set

  val connection = BridgeConnection(BridgeConnection.SERVICE_SYSTEM, "Linux.apk System", this)
  private val settings = HostSettings(context)
  val notifications = Notifications(context, this)
  val clipboard = ClipboardSync(context, this)
  val media = MediaSync(context, this)
  val urls = UrlOpener(this)

  override fun start() {
    thread.start()
    handler = Handler(thread.looper)
    instance = this
    notifications.start()
    clipboard.start()
    media.start()
    connection.start()
    handler.post { BrowserRole.claim(context) }
    context.registerReceiver(
      packages,
      android.content.IntentFilter(android.content.Intent.ACTION_PACKAGE_ADDED).apply {
        addDataScheme("package")
      },
      null,
      handler,
    )
  }

  private val packages =
    object : android.content.BroadcastReceiver() {
      override fun onReceive(c: Context, i: android.content.Intent) {
        val pkg = i.data?.schemeSpecificPart ?: return
        if (!i.getBooleanExtra(android.content.Intent.EXTRA_REPLACING, false)) {
          BrowserRole.onPackageAdded(context, pkg)
        }
      }
    }

  override fun stop() {
    context.unregisterReceiver(packages)
    connection.stop()
    media.stop()
    clipboard.stop()
    instance = null
    thread.quitSafely()
  }

  // The settings and the host clipboard come as events while START is handled, so the reset goes
  // first. START can take a while: the bridge starts each desktop service it needs.
  override fun onConnected() {
    handler.post { settings.reset() }
    connection.send(Wire.START, onReply = ::onStarted) { u32(FEATURES) }
  }

  private fun onStarted(r: BridgeConnection.Reply) {
    if (!r.ok) {
      Log.w(TAG, "the system service refused START: ${r.status}")
      return
    }
    val have = if (r.body.remaining() >= 4) r.body.int else 0
    Log.i(TAG, "started (features 0x${have.toString(16)})")
    handler.post {
      if (have and Wire.F_NOTIFICATIONS != 0) notifications.onConnected()
      media.onConnected()
    }
  }

  override fun onDisconnected() {
    handler.post {
      notifications.onDisconnected()
      media.onDisconnected()
    }
  }

  override fun onEvent(type: Int, body: ByteBuffer, fds: Array<FileDescriptor>) {
    // Blob fds are closed when this returns: read them here, handle the rest on our thread.
    when (type) {
      Wire.EV_SETTINGS -> {
        val s = DesktopSettings.decode(body)
        handler.post { settings.apply(s) }
      }
      Wire.EV_NOTIFY -> {
        val id = body.int
        val what = body.get().toInt() and 0xff
        val action = body.get().toInt() and 0xff
        body.short
        val len = body.int
        val text =
          if (fds.isNotEmpty()) String(Blob.read(fds[0], len, 1 shl 16), Charsets.UTF_8) else null
        handler.post { notifications.onDesktopEvent(id, what, action, text) }
      }
      Wire.EV_CLIP -> {
        val kind = body.get().toInt() and 0xff
        val sensitive = body.get().toInt() != 0
        body.short
        val len = body.int
        if (fds.isEmpty()) return
        val data = Blob.read(fds[0], len, 16 shl 20)
        handler.post { clipboard.onHostClip(kind, sensitive, data) }
      }
      Wire.EV_MEDIA_CMD -> {
        val player = body.int
        val cmd = body.get().toInt() and 0xff
        body.get()
        body.short
        val arg = body.long
        handler.post { media.onCommand(player, cmd, arg) }
      }
      else -> Log.d(TAG, "event 0x${type.toString(16)} ignored")
    }
  }

  companion object {
    const val TAG = "LinuxApkInteg"
    private const val FEATURES =
      Wire.F_SETTINGS or Wire.F_CLIPBOARD or Wire.F_NOTIFICATIONS or Wire.F_MEDIA or Wire.F_OPEN_URI

    /** The running feature, for the components Android starts (listener, activities). */
    @Volatile var instance: IntegrationFeature? = null
  }
}
