// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.input

/** Closed mice remain pending until the bridge acknowledges their removal. */
class EvdevRemovals {
  private val pending = LinkedHashSet<Int>()

  fun add(device: Int) {
    pending.add(device)
  }

  fun flush(remove: (Int) -> Boolean) {
    val it = pending.iterator()
    while (it.hasNext()) {
      if (remove(it.next())) it.remove()
    }
  }
}
