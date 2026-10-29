// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.integration.clipboard

/** A single notification from installing a host file, consumed by any observed selection. */
class ImportedFileEcho {
  private var uris: List<String?> = emptyList()
  private var generation = 0L

  fun install(uris: List<String?>): Long {
    this.uris = uris
    return ++generation
  }

  fun consume(uri: String?, generation: Long): Boolean = consume(listOf(uri), generation)

  fun consume(uris: List<String?>, generation: Long): Boolean {
    val expected = this.uris
    this.uris = emptyList()
    return expected.isNotEmpty() && expected == uris && this.generation == generation
  }
}
