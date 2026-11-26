// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.wm

import dev.sargunv.innkeeper.system.wm.DialogBounds.Rect
import dev.sargunv.innkeeper.system.wm.DialogBounds.Shape
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull

class DialogBoundsTest {
  @Test
  fun aDialogThemedActivityShowsItsOwnFrame() {
    assertEquals(
      Rect(350, 296, 930, 503),
      DialogBounds.crop(1280, 800, listOf(Rect(350, 296, 930, 503))),
    )
  }

  @Test
  fun aTranslucentActivityWindowDoesNotCountButItsDialogsDo() {
    // PackageInstaller: a full-display translucent activity with a dialog and a smaller window.
    val frames = listOf(Rect(0, 0, 1280, 800), Rect(352, 204, 928, 595), Rect(352, 384, 928, 416))
    assertEquals(Rect(352, 204, 928, 595), DialogBounds.crop(1280, 800, frames))
  }

  @Test
  fun onlyFullDisplayWindowsShowEverything() {
    assertNull(DialogBounds.crop(1280, 800, listOf(Rect(0, 0, 1280, 800))))
    assertNull(DialogBounds.crop(1280, 800, emptyList()))
  }

  @Test
  fun framesAreClippedToTheDisplay() {
    assertEquals(
      Rect(0, 700, 400, 800),
      DialogBounds.crop(1280, 800, listOf(Rect(-20, 700, 400, 900))),
    )
    // A window wholly off the display and an empty one count for nothing.
    assertNull(DialogBounds.crop(1280, 800, listOf(Rect(1300, 0, 1400, 10), Rect(5, 5, 5, 9))))
  }

  @Test
  fun windowsThatTogetherCoverTheDisplayShowEverything() {
    val halves = listOf(Rect(0, 0, 640, 800), Rect(640, 0, 1280, 800))
    assertNull(DialogBounds.crop(1280, 800, halves))
  }

  private val full = Rect(0, 0, 1280, 800)
  private val dialog = Rect(350, 296, 930, 503)

  private fun DialogBounds.Latch.next(translucent: Boolean, vararg frames: Rect) =
    next(translucent, 1280, 800, frames.toList())

  @Test
  fun aTranslucentAppsPopupDoesNotShrinkItsWindow() {
    // An app whose activities are all translucent, with its own full-display UI.
    val latch = DialogBounds.Latch()
    assertEquals(Shape.Show(null), latch.next(true, full))
    assertEquals(Shape.Show(null), latch.next(true, full, dialog))
  }

  @Test
  fun aDialogTaskFollowsItsDialogUntilItShowsItsWholeDisplay() {
    val latch = DialogBounds.Latch()
    assertEquals(Shape.Show(dialog), latch.next(true, full, dialog))
    val taller = Rect(350, 200, 930, 600)
    assertEquals(Shape.Show(taller), latch.next(true, full, taller))
    // The task starts an opaque activity, then returns to its dialog.
    assertEquals(Shape.Show(null), latch.next(false, full))
    assertEquals(Shape.Show(null), latch.next(true, full, dialog))
  }

  @Test
  fun aTranslucentTaskWaitsUntilItDraws() {
    val latch = DialogBounds.Latch()
    assertEquals(Shape.Wait, latch.next(true))
    assertEquals(Shape.Show(dialog), latch.next(true, dialog))
  }

  @Test
  fun anOpaqueTaskShowsAtOnceAndNeverCrops() {
    // A translucent trampoline's task turns opaque before anything draws.
    val latch = DialogBounds.Latch()
    assertEquals(Shape.Show(null), latch.next(false))
    assertEquals(Shape.Show(null), latch.next(true, full, dialog))
  }
}
