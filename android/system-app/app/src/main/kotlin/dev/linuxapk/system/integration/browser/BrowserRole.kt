// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.integration.browser

import android.app.role.RoleManager
import android.content.ComponentName
import android.content.Context
import android.content.pm.PackageManager
import android.util.Log

/**
 * Keeps [BrowserActivity] as Android's default browser so ordinary links open on the desktop.
 * Verified App Links still win before the browser role. Custom Tabs may explicitly target an
 * installed Android browser without giving it ordinary links.
 *
 * The image's WebView test shell would otherwise be the browser; its browser activity is turned off
 * (apps that pick a browser themselves, like Wikipedia, would choose it).
 */
object BrowserRole {
  /** Browsers that come with the image and aren't meant for users. */
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
}
