// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.apps

import android.content.ComponentName
import android.content.Context
import android.content.pm.ActivityInfo
import android.content.pm.ApplicationInfo
import android.content.pm.PackageManager
import android.os.Looper
import android.util.Base64
import android.util.TypedValue
import dev.sargunv.innkeeper.system.wm.WindowSizing
import kotlin.system.exitProcess

/**
 * What `innkeeper install` asks Android about an app it installed (host/core/src/android.rs). Runs
 * inside the session under app_process, as root rather than as an app:
 * ```
 * CLASSPATH=/system_ext/priv-app/InnkeeperSystem/InnkeeperSystem.apk \
 *   app_process / dev.sargunv.innkeeper.system.apps.IconDump PKG SIZE...
 * ```
 *
 * Prints, for PKG's launcher activity:
 * ```
 * component <flattened ComponentName>
 * label <label, one line>
 * category <ApplicationInfo.category>
 * splash <AARRGGBB>                 (its theme's splash screen background, if it has a colour)
 * window <width> <height>           (the window it opens in, dp)
 * icon <size> <base64 PNG>          (one line per SIZE)
 * ```
 *
 * The splash colour and window size are for the desktop's launch splash (docs/display.md, "Launch
 * feedback").
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
    for (mime in FileAssociations.mimeTypes(pm, pkg)) out.append("mime $mime\n")
    splashColor(pm, app, launcher)?.let { out.append("splash %08x\n".format(it)) }
    val size = WindowSizing.of(launcher)
    out.append("window ${size.widthDp} ${size.heightDp}\n")
    val icon = AppIcons.load(pm, app, launcher)
    for (size in args.drop(1).map(String::toInt)) {
      val png = Base64.encodeToString(AppIcons.png(icon, size), Base64.NO_WRAP)
      out.append("icon $size $png\n")
    }
    print(out)
    System.out.flush()
    exitProcess(0)
  }

  /**
   * The colour the app's launch window starts with: `windowSplashScreenBackground`, else its window
   * background if that is a colour, else `colorBackground`. Transparent ones (the framework's
   * default splash background, trampoline activities) don't count.
   */
  private fun splashColor(pm: PackageManager, app: ApplicationInfo, a: ActivityInfo?): Int? =
    try {
      val theme = pm.getResourcesForApplication(app).newTheme()
      val res = a?.themeResource?.takeIf { it != 0 } ?: app.theme
      if (res != 0) theme.applyStyle(res, true)
      val v = TypedValue()
      listOf(
          android.R.attr.windowSplashScreenBackground,
          android.R.attr.windowBackground,
          android.R.attr.colorBackground,
        )
        .firstNotNullOfOrNull { attr ->
          v.takeIf {
              theme.resolveAttribute(attr, it, true) &&
                it.type in TypedValue.TYPE_FIRST_COLOR_INT..TypedValue.TYPE_LAST_COLOR_INT &&
                it.data ushr 24 != 0
            }
            ?.data
        }
    } catch (e: Exception) {
      System.err.println("splash colour of ${app.packageName}: $e")
      null
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
