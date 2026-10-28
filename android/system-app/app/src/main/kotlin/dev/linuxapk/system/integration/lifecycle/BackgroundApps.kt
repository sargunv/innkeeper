// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.integration.lifecycle

import android.content.Context

/**
 * The apps the user lets run in the background (off by default): while one is installed, the
 * session doesn't stop when idle, and the desktop shows an indicator. Set through the control
 * receiver's `dev.linuxapk.BACKGROUND` (`linuxapk background`). Any thread.
 */
class BackgroundApps(private val context: Context) {
  private val prefs by lazy {
    context
      .createDeviceProtectedStorageContext()
      .getSharedPreferences("lifecycle", Context.MODE_PRIVATE)
  }

  /** Allowed and still installed, sorted. */
  fun allowed(): List<String> =
    prefs.getStringSet(KEY, emptySet()).orEmpty().filter(::installed).sorted()

  /** Returns an error, or null once set. */
  @Synchronized
  fun set(pkg: String, allowed: Boolean): String? {
    if (allowed && !installed(pkg)) return "unknown package $pkg"
    val s = prefs.getStringSet(KEY, emptySet()).orEmpty().toMutableSet()
    if (allowed) s += pkg else s -= pkg
    prefs.edit().putStringSet(KEY, s).apply()
    return null
  }

  private fun installed(pkg: String) =
    try {
      context.packageManager.getApplicationInfo(pkg, 0)
      true
    } catch (_: Exception) {
      false
    }

  private companion object {
    const val KEY = "background"
  }
}
