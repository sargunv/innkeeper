// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.documents

/** Main-thread gate for connection, timeout, result and activity teardown callbacks. */
class PickerState {
  private var waiting = true
  var done = false
    private set

  fun connected(): Boolean {
    if (!waiting || done) return false
    waiting = false
    return true
  }

  fun finish(): Boolean {
    if (done) return false
    done = true
    return true
  }
}
