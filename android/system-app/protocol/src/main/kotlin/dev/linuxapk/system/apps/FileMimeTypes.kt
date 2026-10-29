// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.apps

/** Concrete shared-mime-info types from IntentFilter's resolved data types. */
fun fileMimeTypes(types: Iterable<String>): List<String> {
  fun token(s: String) =
    s.isNotEmpty() && s.all { it.isLetterOrDigit() && it.code < 128 || it in "!#$&^_.+-" }
  return types
    .filter {
      val parts = it.split('/')
      parts.size == 2 && token(parts[0]) && token(parts[1])
    }
    .distinct()
    .sorted()
}
