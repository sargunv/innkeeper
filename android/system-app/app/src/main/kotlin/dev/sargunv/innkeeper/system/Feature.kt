// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system

/** One self-contained desktop-integration area, run by [InnkeeperSystemApp]. */
interface Feature {
  val name: String

  /**
   * Starts the feature's own threads; must not block. Features run for the life of the persistent
   * process, so there is no stop.
   */
  fun start()
}
