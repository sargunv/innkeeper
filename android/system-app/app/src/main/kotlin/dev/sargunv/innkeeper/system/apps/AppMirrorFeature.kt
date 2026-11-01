// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.apps

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.ApplicationInfo
import android.content.pm.PackageManager
import android.os.Handler
import android.os.HandlerThread
import android.os.SystemProperties
import android.util.Log
import dev.sargunv.innkeeper.system.Feature
import dev.sargunv.innkeeper.system.bridge.Blob
import dev.sargunv.innkeeper.system.bridge.BridgeConnection
import dev.sargunv.innkeeper.system.wm.WindowCommands
import dev.sargunv.innkeeper.system.wm.WindowSizing
import java.io.ByteArrayOutputStream
import java.io.FileDescriptor
import java.nio.ByteBuffer
import kotlin.concurrent.thread
import org.json.JSONObject

/**
 * Apps installed inside Android (an app store, PackageInstaller, `pm install`) get a desktop entry
 * and icons on the host, and lose them on uninstall. Package events go to the bridge's apps
 * service, which writes the entries with the same code as `innkeeper install`
 * (host/bridge/src/apps/mod.rs).
 *
 * Only launchable, non-system apps are mirrored. On every connection the current set is checked
 * (HAVE), so installs made while the bridge was away are caught up. The bridge skips entries that
 * `innkeeper install` or another session profile wrote.
 */
class AppMirrorFeature(private val context: Context, private val settings: AppSettings) : Feature {
  override val name = "apps"

  private val thread = HandlerThread("innkeeper-apps")
  private lateinit var handler: Handler
  private val pm = context.packageManager
  private val windows = WindowCommands(context)
  private val channel: BridgeConnection =
    BridgeConnection(
      BridgeConnection.SERVICE_APPS,
      "Innkeeper System apps",
      object : BridgeConnection.Listener {
        override fun onConnected() = this@AppMirrorFeature.onConnected()

        override fun onEvent(type: Int, body: ByteBuffer, fds: Array<FileDescriptor>) {
          if (type == 0x4502) {
            require(!body.hasRemaining() && fds.isEmpty())
            onConnected()
            return
          }
          if (type != COMMAND) return
          val id = body.long
          val len = body.int
          require(!body.hasRemaining() && fds.size == 1)
          val command = JSONObject(String(Blob.read(fds[0], len, 65536), Charsets.UTF_8))
          val reply = { r: JSONObject ->
            val response = r.toString().toByteArray(Charsets.UTF_8)
            channel.send(RESULT, blob = response) { u64(id).u32(response.size) }
          }
          if (command.getString("action") in WINDOW_ACTIONS) {
            // A launch can wait for its first frame, so it runs off the apps worker.
            thread(name = "innkeeper-windows") { reply(windows.execute(command)) }
          } else {
            handler.post { reply(settings.execute(command)) }
          }
        }
      },
    )

  private val receiver =
    object : BroadcastReceiver() {
      override fun onReceive(c: Context, intent: Intent) {
        val pkg = intent.data?.schemeSpecificPart ?: return
        val replacing = intent.getBooleanExtra(Intent.EXTRA_REPLACING, false)
        when (intent.action) {
          Intent.ACTION_PACKAGE_ADDED,
          Intent.ACTION_PACKAGE_REPLACED,
          Intent.ACTION_PACKAGE_CHANGED -> handler.post { sync(pkg) }
          Intent.ACTION_PACKAGE_FULLY_REMOVED -> if (!replacing) handler.post { removed(pkg) }
        }
      }
    }

  private val bootReceiver =
    object : BroadcastReceiver() {
      override fun onReceive(c: Context, intent: Intent) {
        if (intent.action == Intent.ACTION_BOOT_COMPLETED) reportBootCompleted()
      }
    }

  override fun start() {
    thread.start()
    handler = Handler(thread.looper)
    val f =
      IntentFilter().apply {
        addAction(Intent.ACTION_PACKAGE_ADDED)
        addAction(Intent.ACTION_PACKAGE_REPLACED)
        addAction(Intent.ACTION_PACKAGE_CHANGED)
        addAction(Intent.ACTION_PACKAGE_FULLY_REMOVED)
        addDataScheme("package")
      }
    context.registerReceiver(receiver, f, null, handler, Context.RECEIVER_EXPORTED)
    context.registerReceiver(
      bootReceiver,
      IntentFilter(Intent.ACTION_BOOT_COMPLETED),
      null,
      handler,
      Context.RECEIVER_EXPORTED,
    )
    channel.start()
  }

  private fun onConnected() {
    handler.post {
      // Replay BOOT_COMPLETED after a late process start or reconnect. A connected channel alone
      // doesn't mean that boot has completed.
      reportBootCompleted()
      val launchable =
        pm
          .queryIntentActivities(
            Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER),
            PackageManager.ResolveInfoFlags.of(0),
          )
          .map { it.activityInfo.packageName }
          .toSet()
      for (p in launchable) sync(p)
    }
  }

  private fun reportBootCompleted() {
    if (SystemProperties.get("sys.boot_completed") == "1") channel.send(BOOT_COMPLETED)
  }

  private fun app(pkg: String): ApplicationInfo? {
    if (pkg == context.packageName) return null
    val ai =
      try {
        pm.getApplicationInfo(pkg, PackageManager.ApplicationInfoFlags.of(0))
      } catch (_: PackageManager.NameNotFoundException) {
        return null
      }
    // Preinstalled apps and their updates belong to the image, so they get no desktop entry.
    if (
      ai.flags and (ApplicationInfo.FLAG_SYSTEM or ApplicationInfo.FLAG_UPDATED_SYSTEM_APP) != 0
    ) {
      return null
    }
    if (pm.getLaunchIntentForPackage(pkg) == null) return null
    return ai
  }

  private fun sync(pkg: String) {
    if (!channel.connected) return
    val ai = app(pkg) ?: return
    val version =
      try {
        pm.getPackageInfo(pkg, PackageManager.PackageInfoFlags.of(0)).versionName ?: ""
      } catch (_: PackageManager.NameNotFoundException) {
        return
      }
    val have =
      channel.call(HAVE) {
        name(pkg)
        name(version)
      } ?: return
    have.closeFds()
    if (!have.ok || have.body.remaining() < 1 || have.body.get().toInt() != MISSING) return
    val label = ai.loadLabel(pm).toString()
    val launcher = AppIcons.launcherActivity(pm, pkg)
    val splash = AppIcons.splashColor(pm, ai, launcher)
    val window = WindowSizing.of(launcher)
    val icons = icons(ai)
    val fd = Blob.create(icons)
    try {
      val r =
        channel.call(ADDED, fd) {
          name(pkg)
          name(version)
          name(label)
          i32(ai.category)
          u8(if (ai.category == ApplicationInfo.CATEGORY_GAME) 1 else 0)
          // The launch splash colour (alpha 0 for none) and window size.
          u32(splash?.let { it or (0xff shl 24) } ?: 0)
          u16(window.widthDp)
          u16(window.heightDp)
          // The installed APK, which the bridge reads for localized names and URL schemes.
          line(ai.sourceDir, 1024)
          u32(icons.size)
        }
      r?.closeFds()
      Log.i(
        TAG,
        "$pkg $version: desktop entry ${if (r?.ok == true) "written" else "failed (${r?.status})"}",
      )
    } finally {
      Blob.close(fd)
    }
  }

  private fun removed(pkg: String) {
    val r = channel.call(REMOVED) { name(pkg) }
    r?.closeFds()
    Log.i(TAG, "$pkg: removed (${r?.status})")
  }

  /** The app's icon as PNGs: repeated `u32 size`, `u32 len`, bytes (the bridge's layout). */
  private fun icons(ai: ApplicationInfo): ByteArray {
    val icon = AppIcons.load(pm, ai, AppIcons.launcherActivity(pm, ai.packageName))
    val out = ByteArrayOutputStream(64 * 1024)
    fun u32(v: Int) = repeat(4) { out.write(v ushr (8 * it)) }
    for (size in SIZES) {
      val png = AppIcons.png(icon, size)
      u32(size)
      u32(png.size)
      out.write(png)
    }
    return out.toByteArray()
  }

  companion object {
    const val TAG = "InnkeeperApps"
    private val SIZES = intArrayOf(48, 64, 128, 256, 512)
    private const val HAVE = 0x0501
    private const val ADDED = 0x0502
    private const val REMOVED = 0x0503
    private val WINDOW_ACTIONS = setOf("launch", "windows", "close")
    private const val COMMAND = 0x4501
    private const val RESULT = 0x0504
    private const val BOOT_COMPLETED = 0x0505
    private const val MISSING = 0
  }
}
