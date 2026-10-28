// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.input

import android.util.Log

/**
 * Injects input lines (host/bridge/src/input/proto.rs) read from stdin, through the system app's
 * own translator and injector, for tests:
 * ```
 * CLASSPATH=/system_ext/priv-app/LinuxApkSystem/LinuxApkSystem.apk \
 *   app_process / dev.linuxapk.system.input.StdinInjector [--dry-run]
 * ```
 *
 * `--dry-run` prints each MotionEvent/KeyEvent instead of injecting it; otherwise the caller needs
 * INJECT_EVENTS (root or shell in the session).
 */
object StdinInjector {
  @JvmStatic
  fun main(args: Array<String>) {
    val dryRun =
      when (args.toList()) {
        emptyList<String>() -> false
        listOf("--dry-run") -> true
        else -> {
          System.err.println("usage: StdinInjector [--dry-run]")
          return
        }
      }
    val translator = HostInputClient.newTranslator()
    println("config=${HostInputClient.config()} dryRun=$dryRun")
    val injector = if (dryRun) null else Injector()
    val sink: (OutEvent) -> Unit = { e ->
      val ev = Injector.toInputEvent(e)
      val ok = injector?.inject(e)
      println(if (ok == null) "$ev" else "inject=$ok $ev")
      Log.i(HostInputClient.TAG, "stdin: inject=$ok $ev")
    }
    for (line in System.`in`.bufferedReader(Charsets.UTF_8).lineSequence()) {
      val e = HostEvent.parse(line)
      if (e == null) {
        Log.w(HostInputClient.TAG, "ignoring line: $line")
        continue
      }
      translator.handle(e).forEach(sink)
    }
    translator.reset().forEach(sink)
  }
}
