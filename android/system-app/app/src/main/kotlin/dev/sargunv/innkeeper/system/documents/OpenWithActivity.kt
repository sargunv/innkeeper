// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.documents

import android.app.Activity
import android.app.AlertDialog
import android.content.ClipData
import android.content.ComponentName
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ResolveInfo
import android.graphics.Typeface
import android.os.Bundle
import android.util.Log
import android.view.View
import android.view.ViewGroup
import android.widget.ArrayAdapter
import android.widget.TextView
import dev.sargunv.innkeeper.system.InnkeeperSystemApp
import dev.sargunv.innkeeper.system.R
import dev.sargunv.innkeeper.system.apps.sharedMimeType
import dev.sargunv.innkeeper.system.wm.WindowManagerFeature
import kotlin.concurrent.thread

/**
 * The app chooser for the static Flatpak entry. The files are already read-only document grants.
 */
class OpenWithActivity : Activity() {
  private data class Row(
    val label: CharSequence,
    val app: ResolveInfo? = null,
    val share: Boolean = false,
  )

  override fun onCreate(state: Bundle?) {
    super.onCreate(state)
    val uri = intent.data ?: return finish()
    val uris =
      intent.clipData?.let { clip -> List(clip.itemCount) { clip.getItemAt(it).uri } }
        ?: listOf(uri)
    val types = uris.map { contentResolver.getType(it) ?: "application/octet-stream" }
    val type = sharedMimeType(types)
    val share =
      Intent(if (uris.size == 1) Intent.ACTION_SEND else Intent.ACTION_SEND_MULTIPLE).apply {
        this.type = type
        addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        clipData =
          ClipData("Files", types.distinct().toTypedArray(), ClipData.Item(uris.first())).apply {
            uris.drop(1).forEach { addItem(ClipData.Item(it)) }
          }
        if (uris.size == 1) putExtra(Intent.EXTRA_STREAM, uris.first())
        else putParcelableArrayListExtra(Intent.EXTRA_STREAM, ArrayList(uris))
      }
    fun matching(i: Intent): List<ResolveInfo> =
      packageManager
        .queryIntentActivities(
          i,
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
    fun component(a: ResolveInfo) = ComponentName(a.activityInfo.packageName, a.activityInfo.name)
    fun targets(i: Intent, forType: (String) -> Intent): List<ResolveInfo> {
      // A mixed batch must be accepted in full by the same activity, even if its filter
      // happens to match Android's wildcard query for the batch's common MIME type.
      val accepted =
        types.distinct().map { mime -> matching(forType(mime)).map(::component).toSet() }
      return matching(i)
        .filter { app -> accepted.all { component(app) in it } }
        .distinctBy(::component)
        .sortedBy { it.loadLabel(packageManager).toString().lowercase() }
    }
    fun view(mime: String) = Intent(Intent.ACTION_VIEW).setDataAndType(uri, mime)
    val opens = targets(view(type), ::view)
    val shares = targets(share) { mime -> Intent(share).setType(mime) }
    Log.i(TAG, "chooser type=$type apps=${opens.joinToString { it.activityInfo.packageName }}")
    Log.i(
      TAG,
      "share action=${share.action} files=${uris.size} apps=${shares.joinToString { it.activityInfo.packageName }}",
    )
    val rows = buildList {
      if (opens.isNotEmpty()) {
        add(Row(getString(R.string.open_with_open)))
        opens.forEach { add(Row(it.loadLabel(packageManager), it)) }
      }
      if (shares.isNotEmpty()) {
        add(Row(getString(R.string.open_with_share)))
        shares.forEach { add(Row(it.loadLabel(packageManager), it, true)) }
      }
    }
    val dialog =
      AlertDialog.Builder(this)
        .setTitle(R.string.open_with_title)
        .setNegativeButton(android.R.string.cancel) { _, _ -> finish() }
        .setOnCancelListener { finish() }
    if (rows.isEmpty()) {
      dialog.setMessage(getString(R.string.open_with_empty, type))
    } else {
      val adapter =
        object :
          ArrayAdapter<CharSequence>(
            this,
            android.R.layout.simple_list_item_1,
            rows.map { it.label },
          ) {
          override fun areAllItemsEnabled() = false

          override fun isEnabled(position: Int) = rows[position].app != null

          override fun getView(position: Int, convertView: View?, parent: ViewGroup): View =
            (super.getView(position, convertView, parent) as TextView).apply {
              setTypeface(null, if (rows[position].app == null) Typeface.BOLD else Typeface.NORMAL)
              isEnabled = rows[position].app != null
            }
        }
      dialog.setAdapter(adapter) { _, which ->
        val row = rows[which]
        val activity = row.app!!.activityInfo
        thread(name = "innkeeper-open-with") {
          val error =
            try {
              uris.forEach {
                grantUriPermission(activity.packageName, it, Intent.FLAG_GRANT_READ_URI_PERMISSION)
              }
              val wm =
                (application as InnkeeperSystemApp).windows ?: error("window manager unavailable")
              val target = ComponentName(activity.packageName, activity.name)
              val intents =
                if (row.share) listOf(Intent(share).setComponent(target))
                else
                  uris.mapIndexed { index, file ->
                    Intent(Intent.ACTION_VIEW)
                      .setDataAndType(file, types[index])
                      .setComponent(target)
                      .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
                  }
              for (handoff in intents) {
                val reply =
                  wm.launch(
                    WindowManagerFeature.LaunchRequest(
                      pkg = activity.packageName,
                      launchIntent = handoff,
                    ),
                  )
                check(!reply.startsWith("error:")) { reply }
                Log.i(TAG, "selected ${activity.packageName}: $reply action=${handoff.action}")
              }
              null
            } catch (e: Exception) {
              Log.w(TAG, "handoff failed", e)
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
    private const val TAG = "InnkeeperOpenWith"
  }
}
