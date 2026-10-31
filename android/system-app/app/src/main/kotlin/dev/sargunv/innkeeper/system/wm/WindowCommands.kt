// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.wm

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
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import org.json.JSONObject

/**
 * The window manager's operations for desktop clients, relayed from the bridge's Session1 API
 * (`Launch`, `Windows`, `Close`; docs/bridge-protocol.md) over the apps channel as JSON:
 * ```
 * {"action":"launch","package":P,"component":"","uri":"","grants":["<16 hex>",...],
 *  "token":"","wait":false,"new_window":false,"width":0,"height":0}
 * {"action":"windows"}
 * {"action":"close","display":-1,"package":P}
 * ```
 *
 * The answer is `{"error":"","reply":...}`: for `launch` and `close` one line that starts with the
 * outcome (`launched display=N ...`, `focused ...`, `closed ...`), for `windows` one line per
 * window. `launch` with `wait` answers once a new window shows its first frame (at most
 * [FIRST_FRAME_TIMEOUT_MS]; `shown=no` after the reply if it didn't). Runs on a worker thread,
 * never the UI thread.
 */
class WindowCommands(private val context: Context) {
  fun execute(command: JSONObject): JSONObject =
    try {
      val wm =
        checkNotNull((context.applicationContext as InnkeeperSystemApp).windows) {
          "window manager not running"
        }
      val reply =
        when (command.getString("action")) {
          "launch" -> launch(wm, command)
          "windows" -> wm.list().joinToString("\n")
          "close" ->
            wm.close(
              command.getInt("display").takeIf { it >= 0 },
              command.getString("package").ifEmpty { null },
            )
          else -> error("unknown window action")
        }
      check(!reply.startsWith("error")) { reply.removePrefix("error: ") }
      Log.i(WindowManagerFeature.TAG, "${command.getString("action")}: $reply")
      JSONObject().put("error", "").put("reply", reply)
    } catch (e: Exception) {
      Log.w(WindowManagerFeature.TAG, "window command failed", e)
      val cause = e.cause ?: e
      JSONObject()
        .put("error", (cause.message ?: cause.toString()).take(1024))
        .put("reply", JSONObject.NULL)
    }

  private fun launch(wm: WindowManagerFeature, c: JSONObject): String {
    val pkg = c.getString("package")
    val component =
      c.getString("component")
        .ifEmpty { null }
        ?.let {
          requireNotNull(ComponentName.unflattenFromString(it)) { "bad component $it" }
        }
    val grants = c.getJSONArray("grants")
    val files =
      (0 until grants.length()).map {
        val grant = grants.getString(it)
        require(grant.length == 16 && grant.toULongOrNull(16) != null) { "bad file grant" }
        DocumentsContract.buildDocumentUri(HostDocumentsProvider.AUTHORITY, grant)
      }
    val file = files.firstOrNull()
    if (file != null) {
      // MIME lookup and the app's first open need the documents HELLO after a cold start.
      val ready = CountDownLatch(1)
      val connected = { ready.countDown() }
      HostFiles.whenConnected(connected)
      try {
        check(ready.await(10, TimeUnit.SECONDS)) { "file bridge unavailable" }
      } finally {
        HostFiles.stopWaiting(connected)
      }
      files.forEach { context.grantUriPermission(pkg, it, Intent.FLAG_GRANT_READ_URI_PERMISSION) }
    }
    val reply =
      wm.launch(
        WindowManagerFeature.LaunchRequest(
          pkg,
          component,
          file ?: c.getString("uri").ifEmpty { null }?.let(Uri::parse),
          c.getInt("width"),
          c.getInt("height"),
          c.getString("token"),
          file != null,
          newWindow = c.getBoolean("new_window"),
          launchIntent =
            if (files.size > 1) {
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
    return if (c.getBoolean("wait") && display != null) {
      if (wm.awaitFirstFrame(display, FIRST_FRAME_TIMEOUT_MS)) reply else "$reply shown=no"
    } else {
      reply
    }
  }

  companion object {
    const val FIRST_FRAME_TIMEOUT_MS = 25_000L
    private val LAUNCHED = Regex("^launched display=(\\d+)")
  }
}
