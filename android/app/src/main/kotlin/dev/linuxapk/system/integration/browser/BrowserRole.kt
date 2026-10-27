// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.integration.browser

import android.app.role.RoleManager
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.util.Log

/**
 * Makes [BrowserActivity] Android's default browser when no real browser has the role, so web links
 * open in the host browser. Apps with verified links still get their links (Android checks those
 * before the browser role). A browser the user installs later gets the role from us.
 *
 * The image's WebView test shell would otherwise be the browser; its browser activity is turned off
 * (apps that pick a browser themselves, like Wikipedia, would choose it).
 */
object BrowserRole {
  /** Browsers that come with the image and aren't meant for users. */
  private val TEST_SHELLS = setOf("org.chromium.webview_shell")
  private val TEST_SHELL_ACTIVITIES =
    listOf(
      ComponentName(
        "org.chromium.webview_shell",
        "org.chromium.webview_shell.WebViewBrowserActivity",
      ),
    )

  fun claim(context: Context) {
    for (c in TEST_SHELL_ACTIVITIES) {
      try {
        val pm = context.packageManager
        if (pm.getComponentEnabledSetting(c) != PackageManager.COMPONENT_ENABLED_STATE_DISABLED) {
          pm.setComponentEnabledSetting(
            c,
            PackageManager.COMPONENT_ENABLED_STATE_DISABLED,
            PackageManager.DONT_KILL_APP,
          )
          Log.i(UrlOpener.TAG, "turned off the test browser $c")
        }
      } catch (_: IllegalArgumentException) {
        // Not in this image.
      } catch (e: Exception) {
        Log.w(UrlOpener.TAG, "turning off $c: $e")
      }
    }
    try {
      val rm = context.getSystemService(RoleManager::class.java)
      // Hidden (module) API: RoleManager.get/setBrowserRoleHolder(…, userId),
      // SET_PREFERRED_APPLICATIONS.
      val holder =
        RoleManager::class
          .java
          .getMethod("getBrowserRoleHolder", Int::class.javaPrimitiveType)
          .invoke(rm, 0) as String?
      if (holder == context.packageName) return
      if (holder != null && holder !in TEST_SHELLS) {
        Log.i(UrlOpener.TAG, "default browser is $holder; leaving it")
        return
      }
      val ok =
        RoleManager::class
          .java
          .getMethod(
            "setBrowserRoleHolder",
            String::class.java,
            Int::class.javaPrimitiveType,
          )
          .invoke(rm, context.packageName, 0) as Boolean
      Log.i(UrlOpener.TAG, "default browser: ${holder ?: "none"} -> ${context.packageName} ($ok)")
    } catch (e: Exception) {
      Log.w(UrlOpener.TAG, "browser role: $e")
    }
  }

  /** A browser was installed: it takes the role over from us. */
  fun onPackageAdded(context: Context, pkg: String) {
    if (pkg == context.packageName) return
    val probe =
      Intent(Intent.ACTION_VIEW, Uri.parse("https://example.com/"))
        .addCategory(Intent.CATEGORY_BROWSABLE)
        .setPackage(pkg)
    if (context.packageManager.queryIntentActivities(probe, 0).isEmpty()) return
    try {
      val rm = context.getSystemService(RoleManager::class.java)
      val holder =
        RoleManager::class
          .java
          .getMethod("getBrowserRoleHolder", Int::class.javaPrimitiveType)
          .invoke(rm, 0) as String?
      if (holder != context.packageName) return
      RoleManager::class
        .java
        .getMethod(
          "setBrowserRoleHolder",
          String::class.java,
          Int::class.javaPrimitiveType,
        )
        .invoke(rm, pkg, 0)
      Log.i(UrlOpener.TAG, "default browser -> $pkg (newly installed)")
    } catch (e: Exception) {
      Log.w(UrlOpener.TAG, "browser role: $e")
    }
  }
}
