// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.wm

/** One display's power requests, confined to the window manager handler. */
class WindowVisibility {
  var visible = true
    private set

  /** Null means no change. The owner removes closing windows before dispatching host events. */
  fun update(shown: Boolean): Boolean? {
    if (shown == visible) return null
    visible = shown
    return shown
  }
}
