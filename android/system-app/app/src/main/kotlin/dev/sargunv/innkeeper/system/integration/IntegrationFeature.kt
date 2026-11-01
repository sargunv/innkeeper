// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.integration

import android.content.Context
import android.os.Handler
import android.os.HandlerThread
import android.util.Log
import dev.sargunv.innkeeper.system.Feature
import dev.sargunv.innkeeper.system.bridge.Blob
import dev.sargunv.innkeeper.system.bridge.BridgeConnection
import dev.sargunv.innkeeper.system.bridge.Wire
import dev.sargunv.innkeeper.system.integration.browser.BrowserRole
import dev.sargunv.innkeeper.system.integration.browser.UrlOpener
import dev.sargunv.innkeeper.system.integration.clipboard.ClipboardSync
import dev.sargunv.innkeeper.system.integration.lifecycle.SessionLifecycle
import dev.sargunv.innkeeper.system.integration.media.MediaSync
import dev.sargunv.innkeeper.system.integration.media.PlaybackApps
import dev.sargunv.innkeeper.system.integration.notifications.Notifications
import dev.sargunv.innkeeper.system.integration.settings.DesktopSettings
import dev.sargunv.innkeeper.system.integration.settings.HostSettings
import dev.sargunv.innkeeper.system.wm.WindowManagerFeature
import java.io.FileDescriptor
import java.nio.ByteBuffer

/**
 * Desktop integration over the host bridge's system service (host/bridge/src/system/proto.rs):
 * desktop settings into Android, notifications and media sessions out, the clipboard both ways, web
 * links to the host browser, and what keeps the session up. Events are handled on the feature's own
 * thread.
 */
class IntegrationFeature(private val context: Context, private val wm: WindowManagerFeature?) :
  Feature, BridgeConnection.Listener {
  override val name = "integration"

  private val thread = HandlerThread("innkeeper-integ")
  lateinit var handler: Handler
    private set

  val connection = BridgeConnection(BridgeConnection.SERVICE_SYSTEM, "Innkeeper System", this)
  private val settings = HostSettings(context)
  val notifications = Notifications(context, this, wm)
  val clipboard = ClipboardSync(context, this)
  val media = MediaSync(context, this)
  val playback = PlaybackApps(context, this)
  val urls = UrlOpener(this)
  val lifecycle = SessionLifecycle(context, this, wm)

  override fun start() {
    thread.start()
    handler = Handler(thread.looper)
    instance = this
    notifications.start()
    clipboard.start()
    playback.start()
    lifecycle.start()
    // A media notification shows on the desktop only while its app has no window.
    wm?.addChangeListener {
      handler.post(notifications::refresh)
      lifecycle.poke()
    }
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
          if (pkg != context.packageName) BrowserRole.claim(context)
        }
      }
    }

  // The settings and the host clipboard come as events while START is handled, so the reset goes
  // first. START can take a while because the bridge starts each desktop service it needs.
  override fun onConnected() {
    handler.post { settings.reset() }
    connection.send(Wire.START, onReply = ::onStarted)
  }

  private fun onStarted(r: BridgeConnection.Reply) {
    if (!r.ok) {
      Log.w(TAG, "the system service refused START: ${r.status}")
      return
    }
    val notifies = r.body.remaining() >= 1 && r.body.get().toInt() != 0
    Log.i(TAG, "started (notifications $notifies)")
    handler.post {
      if (notifies) notifications.onConnected()
      media.onConnected()
      playback.onConnected()
      lifecycle.onConnected()
    }
  }

  override fun onDisconnected() {
    handler.post {
      notifications.onDisconnected()
      media.onDisconnected()
      playback.onDisconnected()
    }
  }

  override fun onEvent(type: Int, body: ByteBuffer, fds: Array<FileDescriptor>) {
    // Blob fds are closed when this returns, so read them here and handle the rest on the
    // feature's thread.
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
    const val TAG = "InnkeeperInteg"

    /** The running feature, for the components Android starts (listener, activities). */
    @Volatile var instance: IntegrationFeature? = null
  }
}
