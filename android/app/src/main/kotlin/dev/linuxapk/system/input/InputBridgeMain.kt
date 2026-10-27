// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.input

import android.util.Log

/**
 * Runs the input bridge outside the app, for testing:
 * ```
 * CLASSPATH=/system_ext/priv-app/LinuxApkSystem/LinuxApkSystem.apk \
 *   app_process / dev.linuxapk.system.input.InputBridgeMain [--socket PATH | --stdin] [--dry-run]
 * ```
 *
 * `--stdin` reads protocol lines from stdin (until EOF) instead of the socket. `--dry-run` prints
 * each MotionEvent/KeyEvent instead of injecting it; otherwise the caller needs INJECT_EVENTS (root
 * or shell in the session).
 */
object InputBridgeMain {
  @JvmStatic
  fun main(args: Array<String>) {
    var socket = InputFeature.socketPath()
    var dryRun = false
    var stdin = false
    var i = 0
    while (i < args.size) {
      when (args[i]) {
        "--socket" -> socket = args[++i]
        "--dry-run" -> dryRun = true
        "--stdin" -> stdin = true
        else -> {
          System.err.println("usage: InputBridgeMain [--socket PATH | --stdin] [--dry-run]")
          return
        }
      }
      i++
    }
    val config = InputFeature.config()
    println("${if (stdin) "stdin" else "socket=$socket"} config=$config dryRun=$dryRun")
    val injector = if (dryRun) null else Injector()
    val sink: (OutEvent) -> Unit = { e ->
      val ev = Injector.toInputEvent(e)
      val ok = injector?.inject(e)
      println(if (ok == null) "$ev" else "inject=$ok $ev")
      Log.i(InputBridge.TAG, "main: inject=$ok $ev")
    }
    val translator = { InputTranslator(config, InputFeature::uptimeMillis) }
    if (stdin) {
      val t = translator()
      InputBridge.pump(System.`in`.bufferedReader(Charsets.US_ASCII), t, sink)
      t.reset().forEach(sink)
      return
    }
    InputBridge(socket, translator, sink).start()
    Thread.currentThread().join()
  }
}
