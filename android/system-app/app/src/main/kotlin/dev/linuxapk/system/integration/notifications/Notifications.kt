// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.integration.notifications

import android.app.ActivityOptions
import android.app.Notification
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.RemoteInput
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.drawable.BitmapDrawable
import android.graphics.drawable.Drawable
import android.os.Bundle
import android.os.SystemClock
import android.service.notification.NotificationListenerService
import android.service.notification.StatusBarNotification
import android.util.Log
import dev.linuxapk.system.LinuxApkSystemApp
import dev.linuxapk.system.bridge.Wire
import dev.linuxapk.system.bridge.toRgba
import dev.linuxapk.system.integration.IntegrationFeature
import dev.linuxapk.system.wm.WindowManagerFeature

/**
 * Forwards Android notifications to the desktop (host/bridge/src/system/notify.rs): posted, updated
 * and removed notifications from [NotificationForwarder] become `NOTIFY`/`NOTIFY_CLOSE` requests,
 * and clicks, actions, inline replies and dismissals come back from the desktop. Runs on the
 * integration thread.
 *
 * Not forwarded: group summaries (the children are), media notifications (MPRIS shows those), and
 * notifications from the system itself (`android`, SystemUI).
 */
class Notifications(private val context: Context, private val feature: IntegrationFeature) {
  private class Entry(val id: Int, var sbn: StatusBarNotification, var sentAt: Long = 0) {
    /** Hash of the last blob sent: apps repost unchanged notifications often (timers). */
    var sentHash = 0
  }

  private val byKey = HashMap<String, Entry>()
  private val byId = HashMap<Int, Entry>()
  private var nextId = 1
  private val labels = HashMap<String, String>()
  private val icons = HashMap<String, Wire.Rgba?>()

  @Volatile var listener: NotificationListenerService? = null

  val component = ComponentName(context, NotificationForwarder::class.java)

  /** Grants our own listener access (it's what the desktop sees through). */
  fun start() {
    val nm = context.getSystemService(NotificationManager::class.java)
    try {
      if (!nm.isNotificationListenerAccessGranted(component)) {
        NotificationManager::class
          .java
          .getMethod(
            "setNotificationListenerAccessGranted",
            ComponentName::class.java,
            Boolean::class.javaPrimitiveType,
          )
          .invoke(nm, component, true)
        Log.i(TAG, "notification listener access granted")
      }
    } catch (e: Exception) {
      Log.w(TAG, "granting listener access: $e")
    }
  }

  fun onListenerConnected(l: NotificationListenerService) {
    listener = l
    feature.handler.post { if (feature.connection.connected) postAll() }
    feature.media.onListenerConnected()
  }

  fun onListenerDisconnected() {
    listener = null
  }

  fun onConnected() = postAll()

  fun onDisconnected() {
    // The bridge closed our desktop notifications; post them again on reconnect.
    for (e in byKey.values) e.sentAt = 0
  }

  private fun postAll() {
    val l = listener ?: return
    val active =
      try {
        l.activeNotifications ?: emptyArray()
      } catch (e: Exception) {
        Log.w(TAG, "activeNotifications: $e")
        emptyArray()
      }
    for (sbn in active) posted(sbn, initial = true)
  }

  fun posted(sbn: StatusBarNotification, initial: Boolean = false) {
    if (!wanted(sbn)) {
      // It may have been forwarded before it turned into something we skip.
      byKey[sbn.key]?.let { removed(sbn.key) }
      return
    }
    val e =
      byKey[sbn.key]
        ?: Entry(nextId++, sbn).also {
          byKey[sbn.key] = it
          byId[it.id] = it
        }
    val update = e.sentAt != 0L && !initial
    e.sbn = sbn
    // Progress updates can come many times a second: send at most two per second.
    val wait = e.sentAt + MIN_INTERVAL_MS - SystemClock.uptimeMillis()
    if (wait > 0) {
      feature.handler.removeCallbacksAndMessages(e)
      feature.handler.postAtTime({ send(e, true) }, e, SystemClock.uptimeMillis() + wait)
      return
    }
    send(e, update)
  }

  fun removed(key: String) {
    val e = byKey.remove(key) ?: return
    byId.remove(e.id)
    feature.handler.removeCallbacksAndMessages(e)
    if (e.sentAt != 0L) feature.connection.send(Wire.NOTIFY_CLOSE) { u32(e.id) }
  }

  private fun wanted(sbn: StatusBarNotification): Boolean {
    val n = sbn.notification
    if (sbn.packageName in SKIP_PACKAGES) return false
    if (sbn.packageName == context.packageName) return false
    if (n.flags and Notification.FLAG_GROUP_SUMMARY != 0) return false
    if (n.extras.containsKey(Notification.EXTRA_MEDIA_SESSION)) return false
    return true
  }

  private fun send(e: Entry, update: Boolean) {
    if (!feature.connection.connected) return
    val blob =
      try {
        encode(e.sbn, update)
      } catch (ex: Exception) {
        Log.w(TAG, "encoding ${e.sbn.key}: $ex")
        return
      }
    val hash = blob.contentHashCode()
    if (e.sentAt != 0L && hash == e.sentHash) return
    e.sentHash = hash
    e.sentAt = SystemClock.uptimeMillis()
    feature.connection.send(
      Wire.NOTIFY,
      blob,
      onReply = { if (!it.ok) Log.w(TAG, "notify ${e.id} (${e.sbn.packageName}): ${it.status}") },
    ) {
      u32(e.id).u32(blob.size)
    }
  }

  private fun encode(sbn: StatusBarNotification, update: Boolean): ByteArray {
    val n = sbn.notification
    val x = n.extras
    val title =
      (x.getCharSequence(Notification.EXTRA_TITLE_BIG)
          ?: x.getCharSequence(Notification.EXTRA_TITLE))
        ?.toString()
        .orEmpty()
    val lines = x.getCharSequenceArray(Notification.EXTRA_TEXT_LINES)
    val messages = messages(x)
    val body =
      (x.getCharSequence(Notification.EXTRA_BIG_TEXT)?.toString()
          ?: messages
          ?: lines?.joinToString("\n")
          ?: x.getCharSequence(Notification.EXTRA_TEXT)?.toString())
        .orEmpty()
    val ongoing =
      n.flags and (Notification.FLAG_ONGOING_EVENT or Notification.FLAG_FOREGROUND_SERVICE) != 0
    val channel =
      try {
        listener?.let { l ->
          val ranking = NotificationListenerService.Ranking()
          if (l.currentRanking.getRanking(sbn.key, ranking)) ranking.importance else null
        }
      } catch (_: Exception) {
        null
      } ?: NotificationManager.IMPORTANCE_DEFAULT
    val silent =
      (update && n.flags and Notification.FLAG_ONLY_ALERT_ONCE != 0) ||
        channel < NotificationManager.IMPORTANCE_DEFAULT
    var flags = 0
    if (ongoing) flags = flags or 1
    if (silent) flags = flags or 2
    if (n.contentIntent != null) flags = flags or 4
    val urgency =
      when {
        channel <= NotificationManager.IMPORTANCE_LOW -> 0
        channel >= NotificationManager.IMPORTANCE_HIGH &&
          (n.category == Notification.CATEGORY_CALL || n.category == Notification.CATEGORY_ALARM) ->
          2
        else -> 1
      }
    val max = x.getInt(Notification.EXTRA_PROGRESS_MAX, 0)
    val progress =
      when {
        x.getBoolean(Notification.EXTRA_PROGRESS_INDETERMINATE) -> -2
        max > 0 -> (x.getInt(Notification.EXTRA_PROGRESS) * 100L / max).toInt().coerceIn(0, 100)
        else -> -1
      }
    val actions = (n.actions ?: emptyArray()).take(3)
    val w = Wire.Writer(4096)
    w.u32(1)
      .text(sbn.packageName, 255)
      .text(label(sbn.packageName), 256)
      .text(title, 1024)
      .text(body, 8192)
      .u8(flags)
      .u8(urgency)
      .u16(0)
      .i32(progress)
      .u8(actions.size)
    for (a in actions) {
      val input = a.remoteInputs?.firstOrNull { it.allowFreeFormInput }
      w.text(a.title?.toString(), 128)
        .u8(if (input != null) 1 else 0)
        .text(input?.label?.toString(), 128)
    }
    w.image(appIcon(sbn.packageName)).image(picture(n))
    return w.bytes()
  }

  /** MessagingStyle: the last few messages as "Sender: text" lines. */
  private fun messages(x: Bundle): String? {
    val ms = x.getParcelableArray(Notification.EXTRA_MESSAGES, android.os.Parcelable::class.java)
    if (ms.isNullOrEmpty()) return null
    return Notification.MessagingStyle.Message.getMessagesFromBundleArray(ms)
      .takeLast(5)
      .joinToString("\n") { m ->
        val who = m.senderPerson?.name ?: ""
        if (who.isEmpty()) m.text.toString() else "$who: ${m.text}"
      }
  }

  private fun label(pkg: String): String =
    labels.getOrPut(pkg) {
      try {
        val pm = context.packageManager
        pm.getApplicationInfo(pkg, 0).loadLabel(pm).toString()
      } catch (_: Exception) {
        pkg
      }
    }

  private fun appIcon(pkg: String): Wire.Rgba? =
    icons.getOrPut(pkg) {
      try {
        bitmap(context.packageManager.getApplicationIcon(pkg), 64)?.toRgba(64)
      } catch (_: Exception) {
        null
      }
    }

  private fun picture(n: Notification): Wire.Rgba? {
    val big = n.extras.getParcelable(Notification.EXTRA_PICTURE, Bitmap::class.java)
    if (big != null) return big.toRgba(256)
    val icon = n.getLargeIcon() ?: return null
    return try {
      icon.loadDrawable(context)?.let { bitmap(it, 128) }?.toRgba(128)
    } catch (_: Exception) {
      null
    }
  }

  private fun bitmap(d: Drawable, size: Int): Bitmap? {
    if (d is BitmapDrawable && d.bitmap != null) return d.bitmap
    val w = if (d.intrinsicWidth > 0) minOf(d.intrinsicWidth, size) else size
    val h = if (d.intrinsicHeight > 0) minOf(d.intrinsicHeight, size) else size
    val b = Bitmap.createBitmap(w, h, Bitmap.Config.ARGB_8888)
    d.setBounds(0, 0, w, h)
    d.draw(Canvas(b))
    return b
  }

  // ---- desktop -> Android ----------------------------------------------------

  fun onDesktopEvent(id: Int, what: Int, action: Int, text: String?) {
    val e = byId[id] ?: return
    val sbn = e.sbn
    val n = sbn.notification
    Log.i(TAG, "desktop: notification $id (${sbn.packageName}) event $what action $action")
    when (what) {
      CLICKED -> {
        n.contentIntent?.let { send(it, sbn.packageName, null) }
        if (n.flags and Notification.FLAG_AUTO_CANCEL != 0) cancel(sbn)
      }
      ACTION -> n.actions?.getOrNull(action)?.let { send(it.actionIntent, sbn.packageName, null) }
      REPLY -> {
        val a = n.actions?.getOrNull(action) ?: return
        val inputs = a.remoteInputs ?: return
        val input = inputs.firstOrNull { it.allowFreeFormInput } ?: return
        val fill = Intent()
        RemoteInput.addResultsToIntent(
          arrayOf(input),
          fill,
          Bundle().apply { putCharSequence(input.resultKey, text.orEmpty()) },
        )
        RemoteInput.setResultsSource(fill, RemoteInput.SOURCE_FREE_FORM_INPUT)
        send(a.actionIntent, sbn.packageName, fill)
      }
      DISMISSED -> if (sbn.isClearable) cancel(sbn)
    }
  }

  private fun cancel(sbn: StatusBarNotification) {
    try {
      listener?.cancelNotification(sbn.key)
    } catch (e: Exception) {
      Log.w(TAG, "cancel ${sbn.key}: $e")
    }
  }

  /**
   * Sends [pi]. An activity it starts opens in the app's window if it has one (the window then
   * comes to the front); otherwise on display 0, where the window manager gives it a window.
   */
  private fun send(pi: PendingIntent?, pkg: String, fill: Intent?) {
    if (pi == null) return
    val wm: WindowManagerFeature? = (context.applicationContext as? LinuxApkSystemApp)?.windows
    val display =
      try {
        wm?.list()?.firstOrNull { it.pkg == pkg }?.display
      } catch (_: Exception) {
        null
      }
    val opts =
      ActivityOptions.makeBasic()
        .setPendingIntentBackgroundActivityStartMode(
          ActivityOptions.MODE_BACKGROUND_ACTIVITY_START_ALLOWED,
        )
    if (display != null) opts.launchDisplayId = display
    try {
      pi.send(context, 0, fill, null, null, null, opts.toBundle())
    } catch (e: PendingIntent.CanceledException) {
      Log.w(TAG, "pending intent of $pkg was cancelled")
      return
    }
    if (display != null && pi.isActivity) {
      try {
        wm?.launch(WindowManagerFeature.LaunchRequest(pkg))
      } catch (e: Exception) {
        Log.w(TAG, "focusing $pkg: $e")
      }
    }
  }

  companion object {
    const val TAG = "LinuxApkNotify"
    private const val MIN_INTERVAL_MS = 500L
    private val SKIP_PACKAGES = setOf("android", "com.android.systemui")

    // NOTIFY_EVENT kinds.
    const val CLICKED = 1
    const val ACTION = 2
    const val DISMISSED = 3
    const val REPLY = 4
  }
}
