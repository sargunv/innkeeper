// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.apps

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.ApplicationInfo
import android.content.pm.PackageManager
import android.os.Handler
import android.os.HandlerThread
import android.util.Log
import dev.linuxapk.system.Feature
import dev.linuxapk.system.SystemProps
import dev.linuxapk.system.bridge.Blob
import dev.linuxapk.system.bridge.BridgeConnection
import dev.linuxapk.system.wm.LaunchPolicy
import java.io.ByteArrayOutputStream
import java.io.FileDescriptor
import java.nio.ByteBuffer
import org.json.JSONObject

/**
 * Apps installed inside Android (an app store, PackageInstaller, `pm install`) get a desktop entry
 * and icons on the host, and lose them on uninstall: package events go to the bridge's apps
 * service, which writes the entries with the same code as `linuxapk install`
 * (host/bridge/src/apps/mod.rs).
 *
 * Only launchable, non-system apps are mirrored. On every connection the current set is checked
 * (HAVE), so installs made while the bridge was away are caught up; the bridge skips entries that
 * `linuxapk install` or another session instance wrote.
 */
class AppMirrorFeature(private val context: Context, private val settings: AppSettings) : Feature {
  override val name = "apps"

  private val thread = HandlerThread("linuxapk-apps")
  private lateinit var handler: Handler
  private val pm = context.packageManager
  private val channel: BridgeConnection =
    BridgeConnection(
      BridgeConnection.SERVICE_APPS,
      "Linux.apk System apps",
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
          handler.post {
            val response = settings.execute(command).toString().toByteArray(Charsets.UTF_8)
            channel.send(RESULT, blob = response) { u64(id).u32(response.size) }
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

  override fun stop() {
    channel.stop()
    context.unregisterReceiver(receiver)
    context.unregisterReceiver(bootReceiver)
    thread.quitSafely()
  }

  private fun onConnected() {
    handler.post {
      // Replay after a late process start or reconnect; channel presence alone precedes boot.
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
    if (SystemProps.get("sys.boot_completed") == "1") channel.send(BOOT_COMPLETED)
  }

  private fun app(pkg: String): ApplicationInfo? {
    if (pkg == context.packageName) return null
    val ai =
      try {
        pm.getApplicationInfo(pkg, PackageManager.ApplicationInfoFlags.of(0))
      } catch (_: PackageManager.NameNotFoundException) {
        return null
      }
    // Preinstalled apps (and their updates) belong to the image, not the desktop.
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
          val launcher = AppIcons.launcherActivity(pm, pkg)
          u8(
            if (
              launcher != null &&
                LaunchPolicy.multipleTasks(launcher.launchMode, launcher.documentLaunchMode)
            )
              1
            else 0,
          )
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
    const val TAG = "LinuxApkApps"
    private val SIZES = intArrayOf(48, 64, 128, 256, 512)
    private const val HAVE = 0x0501
    private const val ADDED = 0x0502
    private const val REMOVED = 0x0503
    private const val COMMAND = 0x4501
    private const val RESULT = 0x0504
    private const val BOOT_COMPLETED = 0x0505
    private const val MISSING = 0
  }
}
