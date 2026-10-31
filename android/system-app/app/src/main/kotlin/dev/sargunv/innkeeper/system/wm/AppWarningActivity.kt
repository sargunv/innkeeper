// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.wm

import android.app.Activity
import android.app.ActivityOptions
import android.app.AlertDialog
import android.content.Context
import android.content.Intent
import android.content.SharedPreferences
import android.content.res.Resources
import android.os.Bundle
import android.os.SystemProperties
import android.util.Log

/**
 * The platform's "This app was built for an older version of Android" warning, shown in the app's
 * own window. The platform's AppWarnings dialog always goes to the default display, which has no
 * window, so the window manager turns it off (`debug.wm.disable_deprecated_target_sdk_dialog`) and
 * starts this activity on the app's display instead: same text, and OK hides it for good, like the
 * original's.
 */
class AppWarningActivity : Activity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    val pkg = intent.getStringExtra(EXTRA_PACKAGE) ?: return finish()
    val label =
      try {
        packageManager.getApplicationInfo(pkg, 0).loadLabel(packageManager)
      } catch (e: Exception) {
        pkg
      }
    AlertDialog.Builder(this, android.R.style.Theme_DeviceDefault_Dialog_Alert)
      .setTitle(label)
      .setMessage(systemString("deprecated_target_sdk_message", DEFAULT_MESSAGE))
      .setPositiveButton(android.R.string.ok) { _, _ ->
        prefs(this).edit().putBoolean(KEY_PREFIX + pkg, true).apply()
      }
      .setOnDismissListener { finish() }
      .show()
  }

  companion object {
    private const val TAG = "InnkeeperWm"
    private const val EXTRA_PACKAGE = "package"
    private const val KEY_PREFIX = "warned-deprecated-sdk:"
    private const val DEFAULT_MESSAGE =
      "This app was built for an older version of Android and may not work properly."

    private fun prefs(c: Context) =
      c.createDeviceProtectedStorageContext().getSharedPreferences("wm", Context.MODE_PRIVATE)

    private fun systemString(name: String, default: String): String {
      val res = Resources.getSystem()
      val id = res.getIdentifier(name, "string", "android")
      return if (id != 0) res.getString(id) else default
    }

    // Build.VERSION.MIN_SUPPORTED_TARGET_SDK_INT (hidden) reads this property.
    private val minSupportedTargetSdk: Int by lazy {
      SystemProperties.getInt("ro.build.version.min_supported_target_sdk", 0)
    }

    /** Starts the warning on [display] if [pkg] targets an SDK the platform warns about. */
    fun showIfNeeded(c: Context, prefs: SharedPreferences, pkg: String, display: Int) {
      val target =
        try {
          c.packageManager.getApplicationInfo(pkg, 0).targetSdkVersion
        } catch (e: Exception) {
          return
        }
      if (target >= minSupportedTargetSdk || prefs.getBoolean(KEY_PREFIX + pkg, false)) return
      Log.i(TAG, "$pkg targets SDK $target (< $minSupportedTargetSdk): warning on display $display")
      val intent =
        Intent(c, AppWarningActivity::class.java)
          .putExtra(EXTRA_PACKAGE, pkg)
          .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_MULTIPLE_TASK)
      c.startActivity(intent, ActivityOptions.makeBasic().setLaunchDisplayId(display).toBundle())
    }
  }
}
