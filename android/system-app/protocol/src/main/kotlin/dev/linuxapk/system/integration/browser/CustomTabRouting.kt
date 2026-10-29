// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.integration.browser

/** Null keeps the desktop handoff; only this sign-in flow needs Android's App Link resolver. */
fun customTabBrowser(
  isView: Boolean,
  hasCustomTabSession: Boolean,
  hasVerifiedHosts: Boolean,
  browserPackage: String?,
): String? = if (isView && hasCustomTabSession && hasVerifiedHosts) browserPackage else null
