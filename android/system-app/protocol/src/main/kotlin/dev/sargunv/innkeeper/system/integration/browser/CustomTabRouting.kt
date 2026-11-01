// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.integration.browser

/**
 * Returns null to keep the desktop handoff. Only this sign-in flow needs Android's App Link
 * resolver.
 */
fun customTabBrowser(
  isView: Boolean,
  hasCustomTabSession: Boolean,
  hasVerifiedHosts: Boolean,
  browserPackage: String?,
): String? = if (isView && hasCustomTabSession && hasVerifiedHosts) browserPackage else null
