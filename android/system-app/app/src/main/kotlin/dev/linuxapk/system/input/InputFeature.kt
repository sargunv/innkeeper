// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.input

import android.content.res.Resources
import android.util.Log
import dev.linuxapk.system.Feature
import dev.linuxapk.system.SystemProps

/**
 * Host input: the composer's Wayland seat, injected as Android input events.
 *
 * `persist.linuxapk.input.escape_back` (read at start): send BACK for Escape (default false).
 */
class InputFeature : Feature {
  override val name = "input"

  /** The composer connection; the window manager shares it. */
  @Volatile
  var bridge: InputBridge? = null
    private set

  override fun start() {
    val injector = Injector()
    val config = config()
    var failures = 0
    bridge =
      InputBridge(SOCKET, { InputTranslator(config, ::uptimeMillis) }) { e ->
          if (!injector.inject(e) && failures++ % 100 == 0) {
            Log.w(InputBridge.TAG, "injection refused ($failures so far): $e")
          }
        }
        .also { it.start() }
  }

  override fun stop() {
    bridge?.stop()
    bridge = null
  }

  companion object {
    /** The composer's input stream socket. */
    const val SOCKET = "/dev/socket/linuxapk-input"

    fun config() =
      TranslatorConfig(
        escapeAsBack = SystemProps.getBoolean("persist.linuxapk.input.escape_back", false),
        scrollPixelsPerUnit = scrollFactor(),
      )

    fun uptimeMillis(): Long = android.os.SystemClock.uptimeMillis()

    // ViewConfiguration.getScaledVerticalScrollFactor() without a UI context: the framework's
    // config_verticalScrollFactor (64 dp unless overlaid).
    private fun scrollFactor(): Float {
      val res = Resources.getSystem()
      val id = res.getIdentifier("config_verticalScrollFactor", "dimen", "android")
      return if (id != 0) res.getDimension(id) else 64f * res.displayMetrics.density
    }
  }
}
