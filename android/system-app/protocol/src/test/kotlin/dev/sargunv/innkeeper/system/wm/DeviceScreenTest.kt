// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.wm

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertTrue

class DeviceScreenTest {
  @Test
  fun theScreenIsTheWindowTimesItsScale() {
    assertEquals(DeviceScreen(412, 915, 160), DeviceScreen.of(412, 915, 1.0, 0))
    assertEquals(DeviceScreen(824, 1830, 320), DeviceScreen.of(412, 915, 2.0, 0))
    assertEquals(DeviceScreen(1280, 800, 160), DeviceScreen.of(1280, 800, 1.0, 2))
  }

  @Test
  fun aTurnedWindowKeepsTheDisplaysNaturalOrientation() {
    // Android turned a portrait display a quarter turn, so the window is landscape.
    assertEquals(DeviceScreen(824, 1830, 320), DeviceScreen.of(915, 412, 2.0, 1))
    assertEquals(DeviceScreen(824, 1830, 320), DeviceScreen.of(915, 412, 2.0, 3))
  }

  @Test
  fun aScreenIsLargeFrom600DpOnItsShortSide() {
    assertFalse(DeviceScreen.of(599, 900, 2.0, 0)!!.large)
    assertTrue(DeviceScreen.of(600, 900, 2.0, 0)!!.large)
    assertTrue(DeviceScreen.of(1280, 600, 1.0, 1)!!.large)
    assertEquals(411, DeviceScreen(1080, 2400, 420).smallestWidthDp)
  }

  @Test
  fun badGeometryHasNoScreen() {
    assertNull(DeviceScreen.of(0, 915, 1.0, 0))
    assertNull(DeviceScreen.of(412, 915, Double.NaN, 0))
    assertNull(DeviceScreen.of(4097, 900, 2.0, 0))
    assertEquals(DeviceScreen(8192, 900, 160), DeviceScreen.of(8192, 900, 1.0, 0))
  }

  @Test
  fun theSavedFormRoundTrips() {
    val s = DeviceScreen(824, 1830, 320)
    assertEquals("824x1830@320", s.toString())
    assertEquals("824x1830", s.size)
    assertEquals(s, DeviceScreen.parse(s.toString()))
    val bad = listOf(null, "", "824x1830", "0x1830@320", "824x1830@0", "9000x10@160", "1x1@1x")
    for (s in bad) assertNull(DeviceScreen.parse(s), "$s")
  }
}
