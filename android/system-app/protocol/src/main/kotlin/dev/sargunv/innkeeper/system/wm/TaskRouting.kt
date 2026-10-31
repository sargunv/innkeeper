// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.wm

/** A task keeps its window across activity/package changes and display-0 recovery. */
object TaskRouting {
  data class Window(val display: Int, val task: Int?, val pkg: String = "")

  sealed interface Destination {
    data class Existing(val display: Int) : Destination

    data object NewWindow : Destination

    data object Ignore : Destination
  }

  /** A task can outlive its original root activity and acquire another base package. */
  fun launcherWindow(task: Int?, pkg: String, windows: List<Window>): Int? =
    windows.firstOrNull { task != null && it.task == task }?.display
      ?: windows.firstOrNull { it.task == null && it.pkg == pkg }?.display

  fun destination(task: Int, display: Int, windows: List<Window>): Destination {
    windows
      .firstOrNull { it.task == task }
      ?.let {
        return Destination.Existing(it.display)
      }
    val current = windows.firstOrNull { it.display == display }
    return when {
      current != null && current.task == null -> Destination.Existing(display)
      display == 0 || current != null -> Destination.NewWindow
      // Foreign displays (MediaProjection) and displays held during close are not ours to adopt.
      else -> Destination.Ignore
    }
  }
}
