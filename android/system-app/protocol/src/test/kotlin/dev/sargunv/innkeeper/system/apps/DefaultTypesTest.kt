// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.apps

import kotlin.test.Test
import kotlin.test.assertEquals

class DefaultTypesTest {
  private val view = "android.intent.action.VIEW"

  @Test
  fun aFilterGivesItsTypesElseItsSchemesElseItsActions() {
    assertEquals(
      listOf("android.intent.action.MAIN", "https:", "image/png", "text/plain"),
      defaultTypes(
        listOf(
          PreferredFilter(listOf(view), listOf("text/plain", "image/png"), listOf("content")),
          PreferredFilter(listOf(view), emptyList(), listOf("https")),
          PreferredFilter(listOf("android.intent.action.MAIN"), emptyList(), emptyList()),
          PreferredFilter(listOf(view), listOf("image/png"), emptyList()),
        ),
      ),
    )
    assertEquals(emptyList(), defaultTypes(emptyList()))
  }
}
