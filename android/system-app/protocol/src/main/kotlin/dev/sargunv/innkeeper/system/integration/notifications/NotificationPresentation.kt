// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.integration.notifications

/** ProgressStyle takes precedence over the legacy progress extras. */
data class NotificationProgress(
  val current: Int,
  val max: Int,
  val indeterminate: Boolean = false,
) {
  fun percent(): Int =
    when {
      indeterminate -> -2
      max > 0 -> (current * 100L / max).toInt().coerceIn(0, 100)
      else -> -1
    }
}

fun notificationProgress(legacy: NotificationProgress, style: NotificationProgress?): Int =
  (style ?: legacy).percent()

fun notificationBadge(ongoing: Boolean, rankingAllows: Boolean, channelAllows: Boolean): Boolean =
  !ongoing && rankingAllows && channelAllows
