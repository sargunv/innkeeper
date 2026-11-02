// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.apps

/** The parts of a preferred activity's intent filter that `app info` shows. */
data class PreferredFilter(
  val actions: List<String>,
  val types: List<String>,
  val schemes: List<String>,
)

/**
 * The types that an app opens by default, from its preferred activities. A filter gives its MIME
 * types, or its URI schemes as `scheme:` when it has no types, or its actions when it has neither.
 */
fun defaultTypes(filters: List<PreferredFilter>): List<String> =
  filters
    .flatMap { f -> f.types.ifEmpty { f.schemes.map { "$it:" }.ifEmpty { f.actions } } }
    .distinct()
    .sorted()
