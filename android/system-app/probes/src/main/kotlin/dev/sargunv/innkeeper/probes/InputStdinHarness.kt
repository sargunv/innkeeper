// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.probes

import android.os.SystemClock
import android.util.Log
import android.view.KeyCharacterMap
import dev.sargunv.innkeeper.system.input.HostEvent
import dev.sargunv.innkeeper.system.input.Injector
import dev.sargunv.innkeeper.system.input.InputTranslator
import dev.sargunv.innkeeper.system.input.KeyChars
import dev.sargunv.innkeeper.system.input.OutEvent
import dev.sargunv.innkeeper.system.input.TranslatorConfig

/**
 * Injects input lines (host/bridge/src/input/proto.rs) read from stdin through the system app's
 * translator and injector (the :protocol and :inject modules), for tests:
 * ```
 * CLASSPATH=<InnkeeperProbes.apk in the session> app_process / dev.sargunv.innkeeper.probes.InputStdinHarness [--dry-run]
 * ```
 *
 * `--dry-run` prints each MotionEvent/KeyEvent instead of injecting it; otherwise the caller needs
 * INJECT_EVENTS (root or shell in the session). Escape stays Escape: the system app's per-app
 * Escape setting doesn't apply here. Window shortcuts are on, and a close request is printed (there
 * is no window manager here); text keys reach the app as keys only, since committing their text is
 * the system app's IME.
 */
object InputStdinHarness {
  private const val TAG = "InnkeeperInputHarness"

  @JvmStatic
  fun main(args: Array<String>) {
    val dryRun =
      when (args.toList()) {
        emptyList<String>() -> false
        listOf("--dry-run") -> true
        else -> {
          System.err.println("usage: InputStdinHarness [--dry-run]")
          return
        }
      }
    val config =
      TranslatorConfig(
        scrollPixelsPerUnit = Injector.scrollPixelsPerUnit(),
        keyChars = KeyChars(KeyCharacterMap.load(KeyCharacterMap.VIRTUAL_KEYBOARD)::get),
        windowShortcuts = true,
      )
    val translator =
      InputTranslator(config, SystemClock::uptimeMillis) { display ->
        println("close display=$display")
        Log.i(TAG, "close display=$display")
      }
    println("config=$config dryRun=$dryRun")
    val injector = if (dryRun) null else Injector()
    val sink: (OutEvent) -> Unit = { e ->
      val ev = Injector.toInputEvent(e)
      val ok = injector?.inject(e)
      println(if (ok == null) "$ev" else "inject=$ok $ev")
      Log.i(TAG, "inject=$ok $ev")
    }
    for (line in System.`in`.bufferedReader(Charsets.UTF_8).lineSequence()) {
      val e = HostEvent.parse(line)
      if (e == null) {
        Log.w(TAG, "ignoring line: $line")
        continue
      }
      translator.handle(e).forEach(sink)
    }
    translator.reset().forEach(sink)
  }
}
