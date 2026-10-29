// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.documents

import android.app.Activity
import android.app.AlertDialog
import android.content.ComponentName
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Bundle
import android.util.Log
import dev.linuxapk.system.LinuxApkSystemApp
import dev.linuxapk.system.R
import dev.linuxapk.system.wm.WindowManagerFeature
import kotlin.concurrent.thread

/** Static Flatpak entry's chooser. The file is already a read-only document grant. */
class OpenWithActivity : Activity() {
  override fun onCreate(state: Bundle?) {
    super.onCreate(state)
    val uri = intent.data ?: return finish()
    val type = intent.type ?: "application/octet-stream"
    val view = Intent(Intent.ACTION_VIEW).setDataAndType(uri, type)
    val apps =
      packageManager
        .queryIntentActivities(
          view,
          PackageManager.ResolveInfoFlags.of(PackageManager.MATCH_DEFAULT_ONLY.toLong()),
        )
        .filter {
          val a = it.activityInfo
          a.exported &&
            a.enabled &&
            a.packageName != packageName &&
            (a.permission == null ||
              checkSelfPermission(a.permission) == PackageManager.PERMISSION_GRANTED)
        }
        .distinctBy { it.activityInfo.packageName }
        .sortedBy { it.loadLabel(packageManager).toString().lowercase() }
    Log.i(TAG, "chooser type=$type apps=${apps.joinToString { it.activityInfo.packageName }}")
    val dialog =
      AlertDialog.Builder(this)
        .setTitle(R.string.open_with_title)
        .setNegativeButton(android.R.string.cancel) { _, _ -> finish() }
        .setOnCancelListener { finish() }
    if (apps.isEmpty()) {
      dialog.setMessage(getString(R.string.open_with_empty, type))
    } else {
      dialog.setItems(apps.map { it.loadLabel(packageManager) }.toTypedArray()) { _, which ->
        val activity = apps[which].activityInfo
        thread(name = "linuxapk-open-with") {
          val error =
            try {
              grantUriPermission(activity.packageName, uri, Intent.FLAG_GRANT_READ_URI_PERMISSION)
              val wm =
                (application as LinuxApkSystemApp).windows ?: error("window manager unavailable")
              val reply =
                wm.launch(
                  WindowManagerFeature.LaunchRequest(
                    pkg = activity.packageName,
                    component = ComponentName(activity.packageName, activity.name),
                    uri = uri,
                    file = true,
                  ),
                )
              check(!reply.startsWith("error:")) { reply }
              Log.i(TAG, "selected ${activity.packageName}: $reply")
              null
            } catch (e: Exception) {
              Log.w(TAG, "open failed", e)
              e.message ?: e.toString()
            }
          runOnUiThread {
            if (error == null) finish()
            else
              AlertDialog.Builder(this)
                .setTitle(R.string.open_with_title)
                .setMessage(getString(R.string.open_with_failed, error))
                .setPositiveButton(android.R.string.ok) { _, _ -> finish() }
                .setOnCancelListener { finish() }
                .show()
          }
        }
      }
    }
    dialog.show()
  }

  companion object {
    private const val TAG = "LinuxApkOpenWith"
  }
}
