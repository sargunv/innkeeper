// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.wm

import dev.sargunv.innkeeper.system.wm.DialogBounds.Rect
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
}
