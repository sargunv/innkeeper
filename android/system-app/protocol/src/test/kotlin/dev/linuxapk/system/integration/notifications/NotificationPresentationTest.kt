// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.integration.notifications

import kotlin.test.Test
import kotlin.test.assertEquals

class NotificationPresentationTest {
  @Test
  fun progressUsesStyleBeforeLegacyExtrasIncludingIndeterminateAndBounds() {
    val legacy = NotificationProgress(42, 100)
    for ((value, expected) in
      listOf(
        legacy to 42,
        NotificationProgress(84, 200) to 42,
        NotificationProgress(0, 100, true) to -2,
        NotificationProgress(84, 200, true) to -2,
        NotificationProgress(0, 0) to -1,
        NotificationProgress(-1, 100) to 0,
        NotificationProgress(201, 100) to 100,
      )) {
      assertEquals(expected, notificationProgress(value, null), value.toString())
      assertEquals(expected, notificationProgress(legacy, value), value.toString())
    }
  }

  @Test
  fun badgesRequireBothPermissionsAndExcludeOngoingNotifications() {
    data class Case(
      val ongoing: Boolean,
      val ranking: Boolean,
      val channel: Boolean,
      val badge: Boolean,
    )
    for (c in
      listOf(
        Case(false, true, true, true),
        Case(true, true, true, false),
        Case(false, false, true, false),
        Case(false, true, false, false),
        Case(false, false, false, false),
      )) {
      assertEquals(c.badge, notificationBadge(c.ongoing, c.ranking, c.channel), c.toString())
    }
  }
}
