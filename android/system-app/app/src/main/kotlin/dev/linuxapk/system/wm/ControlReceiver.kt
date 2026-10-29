// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.wm

import android.app.Activity
import android.content.BroadcastReceiver
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.provider.DocumentsContract
import android.util.Log
import dev.linuxapk.system.LinuxApkSystemApp
import dev.linuxapk.system.documents.HostDocumentsProvider
import dev.linuxapk.system.documents.HostFiles
import dev.linuxapk.system.integration.IntegrationFeature
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlin.concurrent.thread

/**
 * The window manager's control interface for the host (the `linuxapk` CLI), run inside the session
 * as root (docs/display.md):
 * ```
 * am broadcast -n dev.linuxapk.system/.wm.ControlReceiver -a dev.linuxapk.LAUNCH --es package PKG \
 *   [--es component PKG/.Activity] [--es uri URI] [--ei width DP --ei height DP] \
 *   [--es token XDG_ACTIVATION_TOKEN] [--ez wait true]
 * am broadcast -n dev.linuxapk.system/.wm.ControlReceiver -a dev.linuxapk.LIST
 * am broadcast -n dev.linuxapk.system/.wm.ControlReceiver -a dev.linuxapk.CLOSE \
 *   (--ei display ID | --es package PKG)
 * am broadcast -n dev.linuxapk.system/.wm.ControlReceiver -a dev.linuxapk.BACKGROUND \
 *   [--es package PKG --ez allowed BOOL]
 * ```
 *
 * LAUNCH with `wait` replies once a new window shows its first frame (at most
 * [FIRST_FRAME_TIMEOUT_MS]; `shown=no` after the reply if it didn't). BACKGROUND sets whether an
 * app may run with no window open, keeping the session up (docs/session.md, "Lifecycle"), and
 * replies with the apps allowed.
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
          if (wm == null) "error: window manager not running" else handle(context, wm, intent)
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

  private fun handle(context: Context, wm: WindowManagerFeature, i: Intent): String =
    when (i.action) {
      ACTION_LAUNCH -> {
        val pkg = i.getStringExtra("package") ?: return "error: --es package PKG is required"
        val component =
          i.getStringExtra("component")?.let {
            ComponentName.unflattenFromString(it) ?: return "error: bad component $it"
          }
        val file =
          i.getStringExtra("file-grant")?.let {
            if (it.length != 16 || it.toULongOrNull(16) == null) return "error: bad file grant"
            DocumentsContract.buildDocumentUri(HostDocumentsProvider.AUTHORITY, it)
          }
        if (file != null) {
          // MIME lookup and the app's first open need the documents HELLO after a cold start.
          // This runs on the control worker, never the UI thread.
          val ready = CountDownLatch(1)
          val connected = { ready.countDown() }
          HostFiles.whenConnected(connected)
          try {
            if (!ready.await(10, TimeUnit.SECONDS)) return "error: file bridge unavailable"
          } finally {
            HostFiles.stopWaiting(connected)
          }
          context.grantUriPermission(pkg, file, Intent.FLAG_GRANT_READ_URI_PERMISSION)
        }
        val reply =
          wm.launch(
            WindowManagerFeature.LaunchRequest(
              pkg,
              component,
              file ?: i.getStringExtra("uri")?.let(Uri::parse),
              i.getIntExtra("width", 0),
              i.getIntExtra("height", 0),
              i.getStringExtra("token").orEmpty(),
              file != null,
            ),
          )
        val display = LAUNCHED.find(reply)?.groupValues?.get(1)?.toInt()
        if (i.getBooleanExtra("wait", false) && display != null) {
          if (wm.awaitFirstFrame(display, FIRST_FRAME_TIMEOUT_MS)) reply else "$reply shown=no"
        } else {
          reply
        }
      }
      ACTION_LIST -> wm.list().joinToString("\n").ifEmpty { "no windows" }
      ACTION_CLOSE -> {
        val display = if (i.hasExtra("display")) i.getIntExtra("display", -1) else null
        wm.close(display, i.getStringExtra("package"))
      }
      ACTION_BACKGROUND -> {
        val lifecycle =
          IntegrationFeature.instance?.lifecycle ?: return "error: integration not running"
        val pkg = i.getStringExtra("package")
        if (pkg != null) {
          if (!i.hasExtra("allowed")) return "error: --ez allowed BOOL is required"
          lifecycle.background.set(pkg, i.getBooleanExtra("allowed", false))?.let {
            return "error: $it"
          }
          lifecycle.poke()
        }
        "background " + lifecycle.background.allowed().joinToString(",").ifEmpty { "-" }
      }
      else -> "error: unknown action ${i.action}"
    }

  companion object {
    const val ACTION_LAUNCH = "dev.linuxapk.LAUNCH"
    const val ACTION_LIST = "dev.linuxapk.LIST"
    const val ACTION_CLOSE = "dev.linuxapk.CLOSE"
    const val ACTION_BACKGROUND = "dev.linuxapk.BACKGROUND"
    const val FIRST_FRAME_TIMEOUT_MS = 25_000L
    private val LAUNCHED = Regex("^launched display=(\\d+)")
  }
}
