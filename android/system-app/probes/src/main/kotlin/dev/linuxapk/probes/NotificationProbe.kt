// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.probes

import android.app.Activity
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.RemoteInput
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.os.Bundle
import android.util.Log

/**
 * Diagnostic: posts a notification with a click target, an action and an inline reply, the way a
 * messaging app does, so the desktop round trip can be tested without one
 * (host/bridge/src/system/notify.rs). Shows nothing.
 *
 * am start -n dev.linuxapk.probes/.NotificationProbe
 *
 * Replies and actions are logged (tag LinuxApkNotifyProbe) and update the notification. Clicking it
 * opens [WindowProbeActivity].
 */
class NotificationProbe : Activity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    val now =
      java.text.DateFormat.getTimeInstance(java.text.DateFormat.SHORT).format(java.util.Date())
    post(this, "Ada Lovelace", "Are we still on for the demo at 3? ($now)")
    finish()
  }

  /** Receives the probe notification's action and reply. */
  class Receiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
      val reply = RemoteInput.getResultsFromIntent(intent)?.getCharSequence(KEY_REPLY)
      when {
        reply != null -> {
          Log.i(TAG, "reply: $reply")
          post(context, "Ada Lovelace", "You replied: $reply")
        }
        intent.action == ACTION_READ -> {
          Log.i(TAG, "action: mark as read")
          context.getSystemService(NotificationManager::class.java).cancel(ID)
        }
      }
    }
  }

  companion object {
    const val TAG = "LinuxApkNotifyProbe"
    private const val CHANNEL = "linuxapk-probe"
    private const val ID = 4242
    private const val KEY_REPLY = "reply"
    private const val ACTION_READ = "dev.linuxapk.probes.READ"
    private const val ACTION_REPLY = "dev.linuxapk.probes.REPLY"

    fun post(context: Context, title: String, text: String) {
      val nm = context.getSystemService(NotificationManager::class.java)
      nm.createNotificationChannel(
        NotificationChannel(CHANNEL, "Notification probe", NotificationManager.IMPORTANCE_DEFAULT),
      )
      fun broadcast(action: String, mutable: Boolean) =
        PendingIntent.getBroadcast(
          context,
          action.hashCode(),
          Intent(action).setClass(context, Receiver::class.java),
          if (mutable) PendingIntent.FLAG_MUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
          else PendingIntent.FLAG_IMMUTABLE,
        )
      val open =
        PendingIntent.getActivity(
          context,
          0,
          Intent(context, WindowProbeActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
          PendingIntent.FLAG_IMMUTABLE,
        )
      val reply =
        Notification.Action.Builder(null, "Reply", broadcast(ACTION_REPLY, true))
          .addRemoteInput(RemoteInput.Builder(KEY_REPLY).setLabel("Message Ada").build())
          .build()
      val read =
        Notification.Action.Builder(null, "Mark as read", broadcast(ACTION_READ, false)).build()
      val n =
        Notification.Builder(context, CHANNEL)
          .setSmallIcon(android.R.drawable.stat_notify_chat)
          .setContentTitle(title)
          .setContentText(text)
          .setContentIntent(open)
          .setAutoCancel(true)
          .addAction(read)
          .addAction(reply)
          .build()
      nm.notify(ID, n)
    }
  }
}
