// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.a11y

import android.content.ComponentName
import android.content.Context
import android.os.Handler
import android.os.Looper
import android.provider.Settings
import android.util.Log
import android.view.accessibility.AccessibilityManager
import dev.sargunv.innkeeper.system.Feature
import dev.sargunv.innkeeper.system.bridge.BridgeConnection
import java.io.FileDescriptor
import java.nio.ByteBuffer

/**
 * Android apps on the desktop's AT-SPI bus (host/bridge/src/a11y/). The bridge's a11y service says
 * whether an assistive technology is on (ACTIVE: the desktop's `org.a11y.Status`); only while it is
 * does this enable [HostAccessibilityService], since an enabled accessibility service changes how
 * apps behave and costs CPU. The service sends each app window's tree, and performs the actions
 * that come back.
 */
class A11yFeature(private val context: Context) : Feature, BridgeConnection.Listener {
  override val name = "a11y"

  val bridge = BridgeConnection(A11yWire.SERVICE, "Innkeeper System a11y", this)

  /** The running service, which registers itself. */
  @Volatile var service: HostAccessibilityService? = null

  private val main = Handler(Looper.getMainLooper())
  private val ours by lazy { ComponentName(context, HostAccessibilityService::class.java) }
  private var active: Boolean? = null // main thread
  private val retry = Runnable { if (active == true && service == null) apply(true) }

  override fun start() {
    // A previous process may have left it on; the bridge says whether it should be.
    setActive(false)
    bridge.start()
  }

  override fun onConnected() {
    val r = bridge.call(A11yWire.START) ?: return
    try {
      if (r.ok && r.body.remaining() >= 1) setActive(r.body.get().toInt() != 0)
    } finally {
      r.closeFds()
    }
    // A restarted bridge has no trees.
    service?.resync()
  }

  override fun onDisconnected() = setActive(false)

  override fun onEvent(type: Int, body: ByteBuffer, fds: Array<FileDescriptor>) {
    when (type) {
      A11yWire.EV_ACTIVE -> if (body.remaining() >= 1) setActive(body.get().toInt() != 0)
      A11yWire.EV_ACTION -> service?.perform(A11yWire.action(body))
    }
  }

  private fun setActive(on: Boolean) {
    main.post {
      if (active == on) return@post
      if (active != null) Log.i(TAG, "assistive technologies ${if (on) "on" else "off"}")
      active = on
      apply(on)
    }
  }

  /**
   * Turns the service on or off. Early in boot AccessibilityManagerService may not list it yet (and
   * drops it from the setting if it is written then), so turning it on waits for that and is
   * repeated until the service connects.
   */
  private fun apply(on: Boolean) {
    main.removeCallbacks(retry)
    if (on && !installed()) {
      main.postDelayed(retry, RETRY_MS)
      return
    }
    setEnabled(on)
    if (on) main.postDelayed(retry, CONNECT_MS)
  }

  private fun installed() =
    context
      .getSystemService(AccessibilityManager::class.java)
      .installedAccessibilityServiceList
      .any { it.resolveInfo.serviceInfo.let { s -> ComponentName(s.packageName, s.name) } == ours }

  /** Adds or removes our service in the enabled list, leaving any others as they are. */
  private fun setEnabled(on: Boolean) {
    val cr = context.contentResolver
    val key = Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES
    val ids = Settings.Secure.getString(cr, key).orEmpty().split(':').filter { it.isNotEmpty() }
    val others = ids.filter { ComponentName.unflattenFromString(it) != ours }
    val next = if (on) others + ours.flattenToShortString() else others
    if (next != ids) Settings.Secure.putString(cr, key, next.joinToString(":"))
    val enabled = if (next.isEmpty()) 0 else 1
    if (Settings.Secure.getInt(cr, Settings.Secure.ACCESSIBILITY_ENABLED, 0) != enabled) {
      Settings.Secure.putInt(cr, Settings.Secure.ACCESSIBILITY_ENABLED, enabled)
    }
  }

  companion object {
    const val TAG = "InnkeeperA11y"
    private const val RETRY_MS = 1000L
    private const val CONNECT_MS = 3000L
  }
}
