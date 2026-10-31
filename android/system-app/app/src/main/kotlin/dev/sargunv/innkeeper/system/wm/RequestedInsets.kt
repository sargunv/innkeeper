// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.wm

import android.content.ComponentName
import android.view.IDisplayWindowInsetsController
import android.view.InsetsSourceControl
import android.view.InsetsState
import android.view.inputmethod.ImeTracker

/** Observe requests, not physical bar visibility: our displays have no system-bar surfaces. */
class RequestedInsets(private val changed: (ComponentName?, Int) -> Unit) :
  IDisplayWindowInsetsController.Stub() {
  override fun topFocusedWindowChanged(component: ComponentName?, requestedVisibleTypes: Int) =
    changed(component, requestedVisibleTypes)

  override fun insetsChanged(state: InsetsState?) {}

  override fun insetsControlChanged(state: InsetsState?, controls: Array<InsetsSourceControl>?) {
    controls?.forEach { it.release { surface -> surface.release() } }
  }

  override fun showInsets(types: Int, token: ImeTracker.Token?) {}

  override fun hideInsets(types: Int, token: ImeTracker.Token?) {}

  override fun setImeInputTargetRequestedVisibility(visible: Boolean, token: ImeTracker.Token?) {}
}
