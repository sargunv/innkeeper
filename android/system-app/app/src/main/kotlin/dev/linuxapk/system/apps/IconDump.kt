// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.apps

import android.content.ComponentName
import android.content.Context
import android.content.pm.PackageManager
import android.os.Looper
import android.util.Base64
import kotlin.system.exitProcess

/**
 * What `linuxapk install` asks Android about an app it installed (host/core/src/android.rs). Runs
 * inside the session under app_process, as root rather than as an app:
 * ```
 * CLASSPATH=/system_ext/priv-app/LinuxApkSystem/LinuxApkSystem.apk \
 *   app_process / dev.linuxapk.system.apps.IconDump PKG SIZE...
 * ```
 *
 * Prints, for PKG's launcher activity:
 * ```
 * component <flattened ComponentName>
 * label <label, one line>
 * category <ApplicationInfo.category>
 * icon <size> <base64 PNG>          (one line per SIZE)
 * ```
 */
object IconDump {
  @JvmStatic
  fun main(args: Array<String>) {
    if (args.isEmpty()) {
      System.err.println("usage: IconDump PKG SIZE...")
      exitProcess(2)
    }
    val pkg = args[0]
    val pm = systemContext().packageManager
    val app = pm.getApplicationInfo(pkg, PackageManager.ApplicationInfoFlags.of(0))
    val launcher = AppIcons.launcherActivity(pm, pkg)

    val out = StringBuilder()
    if (launcher != null) {
      out.append(
        "component ${ComponentName(launcher.packageName, launcher.name).flattenToString()}\n",
      )
    }
    val label = launcher?.loadLabel(pm) ?: app.loadLabel(pm)
    out.append("label ${label.toString().replace(Regex("[\\r\\n\\t]"), " ").trim()}\n")
    out.append("category ${app.category}\n")
    val icon = AppIcons.load(pm, app, launcher)
    for (size in args.drop(1).map(String::toInt)) {
      val png = Base64.encodeToString(AppIcons.png(icon, size), Base64.NO_WRAP)
      out.append("icon $size $png\n")
    }
    print(out)
    System.out.flush()
    exitProcess(0)
  }

  /** The system context of an ActivityThread made for this process (hidden API). */
  private fun systemContext(): Context {
    Looper.prepareMainLooper()
    val at = Class.forName("android.app.ActivityThread")
    val thread = at.getDeclaredConstructor().apply { isAccessible = true }.newInstance()
    at.getDeclaredField("sCurrentActivityThread").apply { isAccessible = true }.set(null, thread)
    return at.getDeclaredMethod("getSystemContext").apply { isAccessible = true }.invoke(thread)
      as Context
  }
}
