// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.integration.lifecycle

import android.app.ActivityManager
import android.app.AlarmManager
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.ApplicationInfo
import android.util.Log
import dev.linuxapk.system.bridge.Wire
import dev.linuxapk.system.integration.IntegrationFeature
import dev.linuxapk.system.wm.WindowManagerFeature

/**
 * What keeps the session up (docs/session.md, "Lifecycle"), sent to the bridge's system service as
 * `LIFECYCLE` whenever it changes: app windows, user apps' running foreground services, the next
 * alarm clock, and the apps allowed to run in the background ([BackgroundApps]). The launcher stops
 * the session after a grace period with none of them, and starts it again for the alarm.
 *
 * Pokes come from window, notification, alarm and package changes; a slow poll catches the rest
 * (services that stop without a notification change). Runs on the integration thread.
 */
class SessionLifecycle(
  private val context: Context,
  private val feature: IntegrationFeature,
  private val wm: WindowManagerFeature?,
) {
  val background = BackgroundApps(context)

  private data class Report(
    val windows: Int,
    val services: Int,
    val wakeupMs: Long,
    val background: List<Pair<String, String>>,
  )

  private var sent: Report? = null
  private val updater = Runnable { update() }
  private val poll =
    object : Runnable {
      override fun run() {
        update()
        feature.handler.postDelayed(this, POLL_MS)
      }
    }

  private val receiver =
    object : BroadcastReceiver() {
      override fun onReceive(c: Context, i: Intent) = poke()
    }

  fun start() {
    context.registerReceiver(
      receiver,
      IntentFilter(AlarmManager.ACTION_NEXT_ALARM_CLOCK_CHANGED),
      null,
      feature.handler,
    )
    context.registerReceiver(
      receiver,
      IntentFilter(Intent.ACTION_PACKAGE_REMOVED).apply { addDataScheme("package") },
      null,
      feature.handler,
    )
    feature.handler.postDelayed(poll, POLL_MS)
  }

  fun stop() {
    context.unregisterReceiver(receiver)
    feature.handler.removeCallbacks(poll)
  }

  /** The bridge has no report of ours yet. */
  fun onConnected() {
    sent = null
    poke()
  }

  /** Something may have changed: report it soon (any thread). */
  fun poke() {
    feature.handler.removeCallbacks(updater)
    feature.handler.postDelayed(updater, DEBOUNCE_MS)
  }

  private fun update() {
    if (!feature.connection.connected) return
    val r =
      Report(
        windows = wm?.packages?.size ?: 0,
        services = foregroundServices(),
        wakeupMs = nextWakeup(),
        background = background.allowed().take(MAX_BACKGROUND).map { it to label(it) },
      )
    if (r == sent) return
    sent = r
    Log.i(TAG, "lifecycle: $r")
    val blob =
      r.background
        .joinToString("") { (pkg, label) -> "$pkg\t${label.replace(CONTROL, " ")}\n" }
        .toByteArray()
    feature.connection.send(
      Wire.LIFECYCLE,
      blob.takeIf { it.isNotEmpty() },
      onReply = {
        if (!it.ok) {
          Log.w(TAG, "LIFECYCLE refused: ${it.status}")
          feature.handler.post { sent = null }
        }
      },
    ) {
      u32(r.windows).u32(r.services).u64(r.wakeupMs).u32(blob.size)
    }
  }

  /** User apps' running foreground services (the system's own don't count). */
  private fun foregroundServices(): Int =
    try {
      @Suppress("DEPRECATION") // all of them, with REAL_GET_TASKS
      context.getSystemService(ActivityManager::class.java).getRunningServices(MAX_SERVICES).count {
        it.foreground && isUserApp(it.service.packageName)
      }
    } catch (e: Exception) {
      Log.w(TAG, "getRunningServices: $e")
      0
    }

  /**
   * The next alarm clock (`setAlarmClock`, what clock apps use), in ms since the epoch; 0: none.
   * Other exact alarms aren't visible through any API.
   */
  private fun nextWakeup(): Long =
    context.getSystemService(AlarmManager::class.java).nextAlarmClock?.triggerTime ?: 0L

  private fun isUserApp(pkg: String): Boolean =
    try {
      context.packageManager.getApplicationInfo(pkg, 0).flags and ApplicationInfo.FLAG_SYSTEM == 0
    } catch (_: Exception) {
      false
    }

  private fun label(pkg: String): String =
    try {
      val pm = context.packageManager
      pm.getApplicationInfo(pkg, 0).loadLabel(pm).toString()
    } catch (_: Exception) {
      pkg
    }

  companion object {
    const val TAG = "LinuxApkLifecycle"
    private const val POLL_MS = 15_000L
    private const val DEBOUNCE_MS = 200L
    private const val MAX_SERVICES = 500
    private const val MAX_BACKGROUND = 32 // the bridge's limit
    private val CONTROL = Regex("\\p{Cntrl}")
  }
}
