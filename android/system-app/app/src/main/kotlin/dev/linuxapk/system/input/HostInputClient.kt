// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.input

import android.content.res.Resources
import android.os.SystemClock
import android.util.Log
import dev.linuxapk.system.Feature
import dev.linuxapk.system.SystemProps
import dev.linuxapk.system.bridge.BridgeConnection
import java.io.FileDescriptor
import java.nio.ByteBuffer
import java.util.concurrent.CopyOnWriteArrayList

/**
 * The bridge's input service (host/bridge/src/input/proto.rs): window requests for the window
 * manager, the host seat injected as Android input events, and the host's input methods for the
 * IME. Input lines are translated and injected on the connection's reader thread; every event also
 * goes to the [Listener]s there.
 *
 * `persist.linuxapk.input.escape_back` (read at start): send BACK for Escape (default false).
 */
class HostInputClient : Feature, BridgeConnection.Listener {
  override val name = "input"

  interface Listener {
    /** Reader thread. */
    fun onHostEvent(e: HostEvent)

    /** A new connection (the bridge restarted), on a thread of its own. */
    fun onConnected() {}
  }

  private val connection =
    BridgeConnection(
      BridgeConnection.SERVICE_INPUT,
      "Linux.apk System input",
      this,
      latencySensitive = true,
    )
  private val listeners = CopyOnWriteArrayList<Listener>()
  private lateinit var injector: Injector
  private lateinit var translator: InputTranslator // reader thread
  private var refused = 0

  override fun start() {
    injector = Injector()
    translator = newTranslator()
    connection.start()
  }

  override fun stop() = connection.stop()

  fun addListener(l: Listener) {
    listeners += l
  }

  fun removeListener(l: Listener) {
    listeners -= l
  }

  /**
   * Asks for a window for display [window] (its size in pixels and density, the window's minimum
   * size in dp), which the next HWC display of that size gets. Returns once the bridge queued it.
   */
  fun requestWindow(
    window: Int,
    width: Int,
    height: Int,
    dpi: Int,
    minWidthDp: Int,
    minHeightDp: Int,
    appId: String,
    title: String,
  ): Boolean {
    val r =
      connection.call(WINDOW_REQUEST, timeoutMs = 2000) {
        u32(window).u32(width).u32(height).u32(dpi).u32(minWidthDp).u32(minHeightDp)
        name(appId)
        name(title.filter { it >= ' ' && it != '\u007f' })
      }
    r?.closeFds()
    if (r?.ok != true) Log.w(TAG, "window for display $window: refused (${r?.status})")
    return r?.ok == true
  }

  fun activateWindow(window: Int) = connection.send(WINDOW_ACTIVATE) { u32(window) }

  fun destroyWindow(window: Int) = connection.send(WINDOW_DESTROY) { u32(window) }

  /** One `ti_*` line for the host input method. Returns false if it wasn't sent. */
  fun textInput(text: String) = connection.send(TEXT_INPUT) { line(text, MAX_TEXT_INPUT_LINE) }

  override fun onConnected() = listeners.forEach { it.onConnected() }

  // Whatever is held (keys, buttons, touches) is released.
  override fun onDisconnected() {
    translator.reset().forEach(::inject)
    translator = newTranslator()
  }

  override fun onEvent(type: Int, body: ByteBuffer, fds: Array<FileDescriptor>) {
    val e =
      when (type) {
        INPUT -> {
          val b = ByteArray(minOf(body.short.toInt() and 0xffff, body.remaining()))
          body.get(b)
          val line = String(b, Charsets.UTF_8)
          val e = HostEvent.parse(line)
          if (e == null) {
            Log.w(TAG, "ignoring line: $line")
            return
          }
          translator.handle(e).forEach(::inject)
          e
        }
        WINDOW_MAPPED -> HostEvent.WindowMapped(body.int)
        WINDOW_UNMAPPED -> HostEvent.WindowUnmapped(body.int)
        else -> return
      }
    for (l in listeners) {
      try {
        l.onHostEvent(e)
      } catch (t: RuntimeException) {
        Log.e(TAG, "listener failed on $e", t)
      }
    }
  }

  private fun inject(e: OutEvent) {
    if (!injector.inject(e) && refused++ % 100 == 0) {
      Log.w(TAG, "injection refused ($refused so far): $e")
    }
  }

  companion object {
    const val TAG = "LinuxApkInput"

    // host/bridge/src/input/proto.rs
    private const val WINDOW_REQUEST = 0x0601
    private const val WINDOW_ACTIVATE = 0x0602
    private const val WINDOW_DESTROY = 0x0603
    private const val TEXT_INPUT = 0x0604
    private const val INPUT = 0x4601
    private const val WINDOW_MAPPED = 0x4602
    private const val WINDOW_UNMAPPED = 0x4603
    private const val MAX_TEXT_INPUT_LINE = 480

    fun newTranslator() = InputTranslator(config(), SystemClock::uptimeMillis)

    fun config() =
      TranslatorConfig(
        escapeAsBack = SystemProps.getBoolean("persist.linuxapk.input.escape_back", false),
        scrollPixelsPerUnit = scrollFactor(),
      )

    // ViewConfiguration.getScaledVerticalScrollFactor() without a UI context: the framework's
    // config_verticalScrollFactor (64 dp unless overlaid).
    private fun scrollFactor(): Float {
      val res = Resources.getSystem()
      val id = res.getIdentifier("config_verticalScrollFactor", "dimen", "android")
      return if (id != 0) res.getDimension(id) else 64f * res.displayMetrics.density
    }
  }
}
