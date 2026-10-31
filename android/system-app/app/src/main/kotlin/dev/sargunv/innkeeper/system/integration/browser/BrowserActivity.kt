// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.integration.browser

import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.verify.domain.DomainVerificationManager
import android.content.pm.verify.domain.DomainVerificationUserState
import android.net.Uri
import android.os.Bundle
import android.util.Log
import android.widget.Toast
import dev.sargunv.innkeeper.system.integration.IntegrationFeature

/**
 * Web links open on the desktop. Custom Tabs from apps with verified App Links use an installed
 * Android browser so their https callbacks can return to the app. Shows nothing and finishes.
 */
class BrowserActivity : Activity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    handle(intent)
    finish()
  }

  private fun handle(intent: Intent?) {
    val uri = intent?.data ?: return
    val browser = androidBrowser(intent)
    if (browser != null) {
      try {
        startActivity(Intent(intent).setComponent(null).setPackage(browser))
        Log.i(UrlOpener.TAG, "Custom Tab in $browser for $launchedFromPackage")
        return
      } catch (e: ActivityNotFoundException) {
        Log.w(UrlOpener.TAG, "Android browser is no longer available", e)
      }
    }
    val ok = IntegrationFeature.instance?.urls?.open(uri.toString()) == true
    if (!ok) {
      Log.w(UrlOpener.TAG, "could not open $uri on the desktop")
      Toast.makeText(this, "Couldn't open the link on the desktop", Toast.LENGTH_SHORT).show()
    }
  }

  private fun androidBrowser(intent: Intent): String? {
    val isView = intent.action == Intent.ACTION_VIEW
    val customTab = intent.hasExtra("android.support.customtabs.extra.SESSION")
    if (!isView || !customTab) return null
    val caller = launchedFromPackage ?: return null
    val state =
      try {
        getSystemService(DomainVerificationManager::class.java)
          .getDomainVerificationUserState(caller)
      } catch (_: PackageManager.NameNotFoundException) {
        null
      }
    val verified =
      state?.isLinkHandlingAllowed == true &&
        state.hostToStateMap.containsValue(DomainVerificationUserState.DOMAIN_STATE_VERIFIED)
    val browser =
      packageManager
        .queryIntentActivities(
          Intent(Intent.ACTION_VIEW, Uri.parse("https://example.com/"))
            .addCategory(Intent.CATEGORY_BROWSABLE),
          // Normal web queries only return the default browser; enumerate alternatives too.
          PackageManager.MATCH_ALL or
            PackageManager.MATCH_DEFAULT_ONLY or
            PackageManager.GET_RESOLVED_FILTER,
        )
        .firstOrNull {
          it.activityInfo.packageName != packageName && it.filter?.countDataAuthorities() == 0
        }
        ?.activityInfo
        ?.packageName
    return customTabBrowser(isView, customTab, verified, browser)
  }
}
