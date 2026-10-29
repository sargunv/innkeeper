// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.input

import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.hardware.display.DisplayManager
import android.net.Uri
import android.os.SystemClock
import android.provider.Settings
import android.util.Log
import android.view.KeyCharacterMap
import android.view.KeyEvent
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
 * goes to the [Listener]s there. The host's key repeat becomes Android's repeat settings.
 *
 * Read at start:
 * - `persist.linuxapk.input.escape_back`: send BACK for Escape (default false).
 * - `persist.linuxapk.input.window_shortcuts`: Ctrl+W and Ctrl+Q close the window, Alt+Left goes
 *   back (default true). Web browsers keep Ctrl+W, which closes a tab.
 */
class HostInputClient(private val context: Context) : Feature, BridgeConnection.Listener {
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
  private var repeat: HostEvent.RepeatInfo? = null // reader thread

  // The text of injected key downs that the IME commits instead (OutKey.text), by down time and
  // scan code, until their key up reaches the IME. Keys for windows without an editor never reach
  // it, so the oldest go.
  private val keyTexts =
    object : LinkedHashMap<Long, String>() {
      override fun removeEldestEntry(eldest: MutableMap.MutableEntry<Long, String>?) =
        size > MAX_KEY_TEXTS
    }

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
   * size in dp), which the next HWC display of that size gets. [token] is the launch's
   * `xdg_activation_v1` token, used when the window shows its first frame (empty: none). Returns
   * once the bridge queued it.
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
    token: String = "",
  ): Boolean {
    val r =
      connection.call(WINDOW_REQUEST, timeoutMs = 2000) {
        u32(window).u32(width).u32(height).u32(dpi).u32(minWidthDp).u32(minHeightDp)
        name(appId)
        name(title.filter { it >= ' ' && it != '\u007f' })
        name(token)
      }
    r?.closeFds()
    if (r?.ok != true) Log.w(TAG, "window for display $window: refused (${r?.status})")
    return r?.ok == true
  }

  /** Focuses a shown window, with the launch's activation [token] if there is one. */
  fun activateWindow(window: Int, token: String = "") =
    connection.send(WINDOW_ACTIVATE) { u32(window).name(token) }

  fun windowPolicy(window: Int, orientation: Int, fullscreen: Boolean, immersive: Boolean) =
    connection.send(WINDOW_POLICY) {
      u32(window).u32(orientation).u32(if (fullscreen) 1 else 0).u32(if (immersive) 1 else 0)
    }

  fun destroyWindow(window: Int) = connection.send(WINDOW_DESTROY) { u32(window) }

  /** One `ti_*` line for the host input method. Returns false if it wasn't sent. */
  fun textInput(text: String) = connection.send(TEXT_INPUT) { line(text, MAX_TEXT_INPUT_LINE) }

  /** What the IME commits for this injected key instead of its character; null: nothing to do. */
  fun keyText(e: KeyEvent): String? = synchronized(keyTexts) { keyTexts[keyTextId(e)] }

  /** The key's up reached the IME. */
  fun keyTextDone(e: KeyEvent) {
    synchronized(keyTexts) { keyTexts.remove(keyTextId(e)) }
  }

  override fun onConnected() = listeners.forEach { it.onConnected() }

  // Whatever is held (keys, buttons, touches) is released.
  override fun onDisconnected() {
    translator.reset().forEach(::inject)
    translator = newTranslator()
    repeat = null
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
          if (e is HostEvent.RepeatInfo) applyRepeat(e)
          translator.handle(e).forEach(::inject)
          e
        }
        WINDOW_MAPPED -> HostEvent.WindowMapped(body.int)
        WINDOW_UNMAPPED -> HostEvent.WindowUnmapped(body.int)
        else -> return
      }
    dispatch(e)
  }

  private fun dispatch(e: HostEvent) {
    for (l in listeners) {
      try {
        l.onHostEvent(e)
      } catch (t: RuntimeException) {
        Log.e(TAG, "listener failed on $e", t)
      }
    }
  }

  // Android's own repeat settings (InputSettings: key_repeat_timeout before the first repeat,
  // key_repeat_delay between repeats, within its bounds); InputDispatcher repeats injected keys.
  private fun applyRepeat(r: HostEvent.RepeatInfo) {
    if (r == repeat) return
    repeat = r
    val cr = context.contentResolver
    fun put(name: String, v: Int) {
      if (Settings.Secure.getInt(cr, name, -1) != v) Settings.Secure.putInt(cr, name, v)
    }
    try {
      put("key_repeat_enabled", if (r.rate > 0) 1 else 0)
      if (r.rate > 0) {
        put("key_repeat_timeout", r.delay.coerceIn(150, 2000))
        put("key_repeat_delay", (1000 / r.rate).coerceIn(20, 2000))
      }
      Log.i(TAG, "key repeat: ${r.rate}/s after ${r.delay} ms")
    } catch (e: RuntimeException) {
      Log.w(TAG, "key repeat settings: $e")
    }
  }

  // Whether the app of an app window's display (named "linuxapk:<package>" by the window manager)
  // opens any https link: a web browser. Asked only on Ctrl+W.
  private fun isBrowserDisplay(display: Int): Boolean {
    val name = context.getSystemService(DisplayManager::class.java).getDisplay(display)?.name
    val pkg = name?.removePrefix(DISPLAY_PREFIX)?.takeIf { it != name } ?: return false
    val link =
      Intent(Intent.ACTION_VIEW, Uri.parse("https://linuxapk.invalid/"))
        .addCategory(Intent.CATEGORY_BROWSABLE)
        .setPackage(pkg)
    return try {
      context.packageManager
        .queryIntentActivities(link, PackageManager.ResolveInfoFlags.of(0))
        .isNotEmpty()
    } catch (e: RuntimeException) {
      Log.w(TAG, "browser check for $pkg: $e")
      false
    }
  }

  private fun inject(e: OutEvent) {
    if (e is OutKey && e.action == KeyEvent.ACTION_DOWN) {
      val text = e.text
      if (text != null)
        synchronized(keyTexts) { keyTexts[keyTextId(e.downTime, e.scanCode)] = text }
    }
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
    private const val WINDOW_POLICY = 0x0605
    private const val INPUT = 0x4601
    private const val WINDOW_MAPPED = 0x4602
    private const val WINDOW_UNMAPPED = 0x4603
    private const val MAX_TEXT_INPUT_LINE = 480
    private const val MAX_KEY_TEXTS = 64
    private const val DISPLAY_PREFIX = "linuxapk:"

    private fun keyTextId(downTime: Long, scanCode: Int) = downTime shl 16 or scanCode.toLong()

    private fun keyTextId(e: KeyEvent) = keyTextId(e.downTime, e.scanCode)
  }

  private fun newTranslator(): InputTranslator {
    val kcm = KeyCharacterMap.load(KeyCharacterMap.VIRTUAL_KEYBOARD)
    return InputTranslator(
      TranslatorConfig(
        escapeAsBack = SystemProps.getBoolean("persist.linuxapk.input.escape_back", false),
        scrollPixelsPerUnit = Injector.scrollPixelsPerUnit(),
        keyChars = KeyChars(kcm::get),
        windowShortcuts = SystemProps.getBoolean("persist.linuxapk.input.window_shortcuts", true),
      ),
      SystemClock::uptimeMillis,
      keepsCtrlW = ::isBrowserDisplay,
      onClose = { dispatch(HostEvent.Close(it)) },
    )
  }
}
