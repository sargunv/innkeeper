// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.input

import android.view.KeyEvent
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse

class PackageInputSettingsTest {
  @Test
  fun packageToggleAppliesLiveAndHeldKeyKeepsItsCode() {
    val stored = mutableMapOf<String, Boolean>()
    val settings = PackageInputSettings({ stored[it] ?: false }, { p, on -> stored[p] = on })
    val packages = mapOf(7 to "app.one", 8 to "app.two")
    val translator = InputTranslator(escapeBack = { settings.escapeBack(packages[it]) })
    fun feed(line: String) =
      translator.handle(requireNotNull(HostEvent.parse(line))).filterIsInstance<OutKey>()
    fun tap() = (feed("key 1 1 1") + feed("key 2 1 0")).map { it.keyCode }
    assertFalse(settings.escapeBack(null))
    feed("keyboard_enter 7")
    assertEquals(listOf(KeyEvent.KEYCODE_ESCAPE, KeyEvent.KEYCODE_ESCAPE), tap())
    settings.setEscapeBack("app.one", true)
    assertEquals(KeyEvent.KEYCODE_BACK, feed("key 3 1 1").single().keyCode)
    settings.setEscapeBack("app.one", false)
    assertEquals(KeyEvent.KEYCODE_BACK, feed("key 4 1 0").single().keyCode)
    assertEquals(listOf(KeyEvent.KEYCODE_ESCAPE, KeyEvent.KEYCODE_ESCAPE), tap())
    settings.setEscapeBack("app.one", true)
    feed("keyboard_enter 8")
    assertEquals(listOf(KeyEvent.KEYCODE_ESCAPE, KeyEvent.KEYCODE_ESCAPE), tap())
    feed("keyboard_enter 7")
    assertEquals(listOf(KeyEvent.KEYCODE_BACK, KeyEvent.KEYCODE_BACK), tap())
    // A new settings reader uses the same persisted package values.
    assertEquals(
      true,
      PackageInputSettings({ stored[it] ?: false }, { _, _ -> }).escapeBack("app.one"),
    )
  }
}
