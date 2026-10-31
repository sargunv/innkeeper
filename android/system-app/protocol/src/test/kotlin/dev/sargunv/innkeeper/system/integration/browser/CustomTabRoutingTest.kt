// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.integration.browser

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull

class CustomTabRoutingTest {
  @Test
  fun onlyCustomTabViewWithVerifiedHostsAndAnAndroidBrowserStaysInAndroid() {
    val browser = "org.mozilla.fennec_fdroid"
    assertEquals(browser, customTabBrowser(true, true, true, browser))
    assertNull(customTabBrowser(false, true, true, browser), "not a VIEW")
    assertNull(customTabBrowser(true, false, true, browser), "plain link from the same app")
    assertNull(customTabBrowser(true, true, false, browser), "no verified App Link hosts")
    assertNull(customTabBrowser(true, true, true, null), "no installed Android browser")
  }
}
