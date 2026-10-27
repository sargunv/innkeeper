// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.ime

import android.content.ComponentName
import android.content.Context
import android.provider.Settings
import android.util.Log
import dev.linuxapk.system.Feature

/**
 * Makes [HostImeService] the default input method, so text fields use the host keyboard and input
 * methods and no on-screen keyboard appears (docs/input.md, "Text input"). It is selected at every
 * start unless the user picked an input method other than the image's stock LatinIME.
 */
class ImeFeature(private val context: Context) : Feature {
  override val name = "ime"

  override fun start() {
    val ours = ComponentName(context, HostImeService::class.java).flattenToShortString()
    val cr = context.contentResolver
    val enabled = Settings.Secure.getString(cr, Settings.Secure.ENABLED_INPUT_METHODS).orEmpty()
    val ids = enabled.split(':').filter { it.isNotEmpty() }
    if (ids.none { it.substringBefore(';') == ours }) {
      Settings.Secure.putString(
        cr,
        Settings.Secure.ENABLED_INPUT_METHODS,
        (ids + ours).joinToString(":"),
      )
    }
    val current = Settings.Secure.getString(cr, Settings.Secure.DEFAULT_INPUT_METHOD)
    if (current == ours) {
      Log.i(TAG, "input method: $current")
      return
    }
    if (current.isNullOrEmpty() || current.startsWith(STOCK_PREFIX)) {
      Settings.Secure.putString(cr, Settings.Secure.DEFAULT_INPUT_METHOD, ours)
      Settings.Secure.putString(cr, Settings.Secure.SELECTED_INPUT_METHOD_SUBTYPE, "-1")
      Log.i(TAG, "input method: $ours (was $current)")
    } else {
      Log.i(TAG, "input method: $current, chosen by the user; leaving it")
    }
  }

  override fun stop() {}

  companion object {
    const val TAG = HostImeService.TAG
    private const val STOCK_PREFIX = "com.android.inputmethod.latin/"
  }
}
