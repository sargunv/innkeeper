// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system

/** One self-contained desktop-integration area, run by [LinuxApkSystemApp]. */
interface Feature {
  val name: String

  /** Starts the feature's own threads; must not block. */
  fun start()

  fun stop()
}
