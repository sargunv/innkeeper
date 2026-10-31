// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.wm

import android.app.Activity
import android.content.BroadcastReceiver
import android.content.ClipData
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.provider.DocumentsContract
import android.util.Log
import dev.sargunv.innkeeper.system.InnkeeperSystemApp
import dev.sargunv.innkeeper.system.documents.HostDocumentsProvider
import dev.sargunv.innkeeper.system.documents.HostFiles
import dev.sargunv.innkeeper.system.integration.IntegrationFeature
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlin.concurrent.thread

/**
 * The window manager's control interface for the host (the `innkeeper` CLI), run inside the session
 * as root (docs/display.md):
 * ```
 * am broadcast -n dev.sargunv.innkeeper.system/.wm.ControlReceiver -a dev.sargunv.innkeeper.LAUNCH --es package PKG \
 *   [--es component PKG/.Activity] [--es uri URI] [--ei width DP --ei height DP] \
 *   [--es token XDG_ACTIVATION_TOKEN] [--ez wait true] [--ez new-window true]
 * am broadcast -n dev.sargunv.innkeeper.system/.wm.ControlReceiver -a dev.sargunv.innkeeper.LIST
 * am broadcast -n dev.sargunv.innkeeper.system/.wm.ControlReceiver -a dev.sargunv.innkeeper.CLOSE \
 *   (--ei display ID | --es package PKG)
 * am broadcast -n dev.sargunv.innkeeper.system/.wm.ControlReceiver -a dev.sargunv.innkeeper.BACKGROUND \
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
 * RESULT_CANCELED (0) on error. The receiver requires
 * `dev.sargunv.innkeeper.permission.CONTROL_WINDOWS` (signature), which root holds implicitly.
 */
class ControlReceiver : BroadcastReceiver() {
  override fun onReceive(context: Context, intent: Intent) {
    val wm = (context.applicationContext as InnkeeperSystemApp).windows
    val result = goAsync()
    thread(name = "innkeeper-control") {
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
        val files =
          i.getStringExtra("file-grant")?.let {
            it.split(',').map { grant ->
              if (grant.length != 16 || grant.toULongOrNull(16) == null)
                return "error: bad file grant"
              DocumentsContract.buildDocumentUri(HostDocumentsProvider.AUTHORITY, grant)
            }
          }
        val file = files?.firstOrNull()
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
          files.forEach {
            context.grantUriPermission(pkg, it, Intent.FLAG_GRANT_READ_URI_PERMISSION)
          }
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
              newWindow = i.getBooleanExtra("new-window", false),
              launchIntent =
                if (files != null && files.size > 1) {
                  Intent(Intent.ACTION_VIEW)
                    .setDataAndType(file, context.contentResolver.getType(files.first()))
                    .apply {
                      setComponent(component)
                      setPackage(pkg)
                      addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
                      clipData =
                        ClipData.newUri(context.contentResolver, "Files", files.first()).apply {
                          files.drop(1).forEach { addItem(ClipData.Item(it)) }
                        }
                    }
                } else null,
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
    const val ACTION_LAUNCH = "dev.sargunv.innkeeper.LAUNCH"
    const val ACTION_LIST = "dev.sargunv.innkeeper.LIST"
    const val ACTION_CLOSE = "dev.sargunv.innkeeper.CLOSE"
    const val ACTION_BACKGROUND = "dev.sargunv.innkeeper.BACKGROUND"
    const val FIRST_FRAME_TIMEOUT_MS = 25_000L
    private val LAUNCHED = Regex("^launched display=(\\d+)")
  }
}
