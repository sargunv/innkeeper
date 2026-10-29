// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.integration.clipboard

/** A single notification from installing a host file, consumed by any observed selection. */
class ImportedFileEcho {
  private var uri: String? = null
  private var generation = 0L

  fun install(uri: String?): Long {
    this.uri = uri
    return ++generation
  }

  fun consume(uri: String?, generation: Long): Boolean {
    val expected = this.uri
    this.uri = null
    return expected != null && expected == uri && this.generation == generation
  }
}
