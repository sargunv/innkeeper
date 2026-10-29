// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.input

/** Per-package Escape policy, read for every new key press; held keys keep their original code. */
class PackageInputSettings(
  private val read: (String) -> Boolean,
  private val write: (String, Boolean) -> Unit,
) {
  fun escapeBack(packageName: String?): Boolean = packageName?.let(read) ?: false

  fun setEscapeBack(packageName: String, enabled: Boolean) = write(packageName, enabled)
}
