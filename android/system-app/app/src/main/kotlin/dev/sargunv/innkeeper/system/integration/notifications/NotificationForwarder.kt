// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.integration.notifications

import android.service.notification.NotificationListenerService
import android.service.notification.StatusBarNotification
import android.util.Log
import dev.sargunv.innkeeper.system.integration.IntegrationFeature

/**
 * The system app's notification listener, which hands every posted and removed notification to
 * [Notifications] on the integration thread. The app grants itself access at startup. While the
 * listener is enabled, [dev.sargunv.innkeeper.system.integration.media.MediaSync] can also see
 * media sessions.
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
    if (Log.isLoggable(Notifications.TAG, Log.DEBUG)) Log.d(Notifications.TAG, "posted ${sbn.key}")
    val f = feature ?: return
    f.handler.post { f.notifications.posted(sbn) }
    f.lifecycle.poke()
  }

  override fun onNotificationRankingUpdate(rankingMap: RankingMap) {
    val f = feature ?: return
    f.handler.post { f.notifications.refresh() }
  }

  override fun onNotificationRemoved(sbn: StatusBarNotification) {
    val f = feature ?: return
    f.handler.post { f.notifications.removed(sbn.key) }
    f.lifecycle.poke()
  }
}
