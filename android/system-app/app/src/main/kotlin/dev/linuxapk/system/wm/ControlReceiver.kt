// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.wm

import android.app.Activity
import android.content.BroadcastReceiver
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.util.Log
import dev.linuxapk.system.LinuxApkSystemApp
import kotlin.concurrent.thread

/**
 * The window manager's control interface for the host (the `linuxapk` CLI), run inside the session
 * as root (docs/display.md):
 * ```
 * am broadcast -n dev.linuxapk.system/.wm.ControlReceiver -a dev.linuxapk.LAUNCH --es package PKG \
 *   [--es component PKG/.Activity] [--es uri URI] [--ei width DP --ei height DP]
 * am broadcast -n dev.linuxapk.system/.wm.ControlReceiver -a dev.linuxapk.LIST
 * am broadcast -n dev.linuxapk.system/.wm.ControlReceiver -a dev.linuxapk.CLOSE \
 *   (--ei display ID | --es package PKG)
 * ```
 *
 * The reply is the broadcast's result data (`am` prints `Broadcast completed: result=-1,
 * data="..."`): one line per window for LIST, else one line that starts with the outcome
 * (`launched`, `focused`, `closed`) or `error:`. The result code is RESULT_OK (-1) on success and
 * RESULT_CANCELED (0) on error. The receiver requires `dev.linuxapk.permission.CONTROL_WINDOWS`
 * (signature), which root holds implicitly.
 */
class ControlReceiver : BroadcastReceiver() {
  override fun onReceive(context: Context, intent: Intent) {
    val wm = (context.applicationContext as LinuxApkSystemApp).windows
    val result = goAsync()
    thread(name = "linuxapk-control") {
      val reply =
        try {
          if (wm == null) "error: window manager not running" else handle(wm, intent)
        } catch (e: Exception) {
          Log.e(WindowManagerFeature.TAG, "control ${intent.action} failed", e)
          "error: ${e.cause ?: e}"
        }
      Log.i(WindowManagerFeature.TAG, "control ${intent.action}: $reply")
      val code = if (reply.startsWith("error")) Activity.RESULT_CANCELED else Activity.RESULT_OK
      result.setResult(code, reply, null)
      result.finish()
    }
  }

  private fun handle(wm: WindowManagerFeature, i: Intent): String =
    when (i.action) {
      ACTION_LAUNCH -> {
        val pkg = i.getStringExtra("package") ?: return "error: --es package PKG is required"
        val component =
          i.getStringExtra("component")?.let {
            ComponentName.unflattenFromString(it) ?: return "error: bad component $it"
          }
        wm.launch(
          WindowManagerFeature.LaunchRequest(
            pkg,
            component,
            i.getStringExtra("uri")?.let(Uri::parse),
            i.getIntExtra("width", 0),
            i.getIntExtra("height", 0),
          ),
        )
      }
      ACTION_LIST -> wm.list().joinToString("\n").ifEmpty { "no windows" }
      ACTION_CLOSE -> {
        val display = if (i.hasExtra("display")) i.getIntExtra("display", -1) else null
        wm.close(display, i.getStringExtra("package"))
      }
      else -> "error: unknown action ${i.action}"
    }

  companion object {
    const val ACTION_LAUNCH = "dev.linuxapk.LAUNCH"
    const val ACTION_LIST = "dev.linuxapk.LIST"
    const val ACTION_CLOSE = "dev.linuxapk.CLOSE"
  }
}
