// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.wm

import android.content.Context
import android.content.pm.PackageManager
import android.util.Log
import android.view.InputWindowHandle
import android.window.WindowInfosListener

/**
 * Reports the frames of the visible app windows on each display, from SurfaceFlinger's window infos
 * (ACCESS_SURFACE_FLINGER). TaskInfo has only the top activity's main window, and a translucent
 * activity's dialogs are windows of their own.
 */
class WindowFrames(private val changed: (Map<Int, List<DialogBounds.Rect>>) -> Unit) :
  WindowInfosListener() {
  private var last: Map<Int, List<DialogBounds.Rect>>? = null

  fun start(context: Context) {
    // Without the permission, registering aborts the process.
    if (context.checkSelfPermission(ACCESS_SURFACE_FLINGER) != PackageManager.PERMISSION_GRANTED) {
      Log.w(
        WindowManagerFeature.TAG,
        "window infos: no $ACCESS_SURFACE_FLINGER, dialogs fill their windows",
      )
      return
    }
    try {
      report(register().first)
    } catch (e: Exception) {
      Log.w(WindowManagerFeature.TAG, "window infos: $e")
    }
  }

  override fun onWindowInfosChanged(
    windowHandles: Array<InputWindowHandle>,
    displayInfos: Array<DisplayInfo>,
  ) = report(windowHandles)

  // On a binder thread or the caller's thread.
  @Synchronized
  private fun report(handles: Array<InputWindowHandle>) {
    val frames =
      handles
        .filter {
          it.layoutParamsType in FIRST_APPLICATION_WINDOW..LAST_SUB_WINDOW &&
            it.inputConfig and INPUT_CONFIG_NOT_VISIBLE == 0
        }
        .groupBy({ it.displayId }) {
          DialogBounds.Rect(it.frame.left, it.frame.top, it.frame.right, it.frame.bottom)
        }
    if (frames == last) return
    last = frames
    changed(frames)
  }

  companion object {
    // WindowManager.LayoutParams.FIRST_APPLICATION_WINDOW and LAST_SUB_WINDOW: activity windows,
    // their dialogs and their panels.
    private const val FIRST_APPLICATION_WINDOW = 1
    private const val LAST_SUB_WINDOW = 1999
    private const val INPUT_CONFIG_NOT_VISIBLE = 1 shl 1 // android.os.InputConfig (hidden)
    private const val ACCESS_SURFACE_FLINGER = "android.permission.ACCESS_SURFACE_FLINGER"
  }
}
