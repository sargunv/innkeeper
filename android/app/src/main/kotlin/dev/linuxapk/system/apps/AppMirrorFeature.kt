// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.apps

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.ApplicationInfo
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.Canvas
import android.os.Handler
import android.os.HandlerThread
import android.util.Log
import dev.linuxapk.system.Feature
import dev.linuxapk.system.host.HostChannel
import dev.linuxapk.system.integration.Blob
import java.io.ByteArrayOutputStream

/**
 * Apps installed inside Android (an app store, PackageInstaller, `pm install`) get a desktop entry
 * and icons on the host, and lose them on uninstall: package events go to the bridge's apps
 * service, which writes the entries with the same code as `linuxapk install` (docs/bridge.md, "Apps
 * service").
 *
 * Only launchable, non-system apps are mirrored. On every connection the current set is checked
 * (HAVE), so installs made while the bridge was away are caught up; the bridge skips entries that
 * `linuxapk install` or another session instance wrote.
 */
class AppMirrorFeature(private val context: Context) : Feature {
  override val name = "apps"

  private val thread = HandlerThread("linuxapk-apps")
  private lateinit var handler: Handler
  private val pm = context.packageManager
  private val channel =
    HostChannel(HostChannel.SERVICE_APPS, "Linux.apk System apps", onConnected = ::onConnected)

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
    channel.start()
  }

  override fun stop() {
    channel.stop()
    context.unregisterReceiver(receiver)
    thread.quitSafely()
  }

  private fun onConnected() {
    handler.post {
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
    val d = pm.getApplicationIcon(ai)
    val out = ByteArrayOutputStream(64 * 1024)
    fun u32(v: Int) = repeat(4) { out.write(v ushr (8 * it)) }
    for (size in SIZES) {
      val bmp = Bitmap.createBitmap(size, size, Bitmap.Config.ARGB_8888)
      d.setBounds(0, 0, size, size)
      d.draw(Canvas(bmp))
      val png = ByteArrayOutputStream()
      bmp.compress(Bitmap.CompressFormat.PNG, 100, png)
      bmp.recycle()
      u32(size)
      u32(png.size())
      png.writeTo(out)
    }
    return out.toByteArray()
  }

  companion object {
    const val TAG = "LinuxApkApps"
    private val SIZES = intArrayOf(48, 64, 128, 256, 512)
    private const val HAVE = 0x0501
    private const val ADDED = 0x0502
    private const val REMOVED = 0x0503
    private const val MISSING = 0
  }
}
