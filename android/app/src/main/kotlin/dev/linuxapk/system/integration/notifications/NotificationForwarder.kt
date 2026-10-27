// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.integration.notifications

import android.service.notification.NotificationListenerService
import android.service.notification.StatusBarNotification
import android.util.Log
import dev.linuxapk.system.integration.IntegrationFeature

/**
 * The system app's notification listener: hands every posted and removed notification to
 * [Notifications] on the integration thread. Access is granted by the app itself at startup. Its
 * being enabled also lets [dev.linuxapk.system.integration.media.MediaSync] see media sessions.
 */
class NotificationForwarder : NotificationListenerService() {
  private val feature
    get() = IntegrationFeature.instance

  override fun onListenerConnected() {
    Log.i(Notifications.TAG, "listener connected")
    feature?.notifications?.onListenerConnected(this)
  }

  override fun onListenerDisconnected() {
    feature?.notifications?.onListenerDisconnected()
  }

  override fun onNotificationPosted(sbn: StatusBarNotification) {
    Log.d(Notifications.TAG, "onNotificationPosted ${sbn.key}")
    val f = feature ?: return
    f.handler.post { f.notifications.posted(sbn) }
  }

  override fun onNotificationRemoved(sbn: StatusBarNotification) {
    val f = feature ?: return
    f.handler.post { f.notifications.removed(sbn.key) }
  }
}
