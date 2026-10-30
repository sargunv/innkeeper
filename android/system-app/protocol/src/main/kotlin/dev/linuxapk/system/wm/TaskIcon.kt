// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.wm

/** Task appearance overrides the launcher icon; only an app without either has no icon. */
object TaskIcon {
  fun <T> choose(task: () -> T?, app: () -> T?): T? = task() ?: app()
}
