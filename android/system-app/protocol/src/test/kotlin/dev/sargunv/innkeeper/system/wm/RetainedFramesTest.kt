// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.wm

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class RetainedFramesTest {
  @Test
  fun retainsUntilReleaseAndHandlesEarlyRelease() {
    val closed = mutableListOf<String>()
    val frames = RetainedFrames<String>(3) { closed += it }
    frames.acquire(1, "first")
    assertTrue(closed.isEmpty())
    frames.release(1)
    assertEquals(listOf("first"), closed)
    frames.release(2)
    frames.acquire(2, "early")
    assertEquals(listOf("first", "early"), closed)
    frames.acquire(2, "reuse")
    assertEquals(2, closed.size)
    frames.clear()
    assertEquals(listOf("first", "early", "reuse"), closed)
    frames.clear()
    assertEquals(3, closed.size)
  }

  @Test
  fun unleasedImagesAndLateReleaseDoNotCloseANewerFrame() {
    val closed = mutableListOf<String>()
    val frames = RetainedFrames<String>(3) { closed += it }
    frames.acquire(0, "not submitted")
    frames.acquire(10, "old frame")
    frames.release(10)
    // The allocation can be reused, but every presentation has a new token.
    frames.acquire(11, "new frame")
    frames.release(10)
    assertEquals(listOf("not submitted", "old frame"), closed)
    frames.release(11)
    assertEquals(listOf("not submitted", "old frame", "new frame"), closed)
    frames.acquire(12, "first held")
    frames.acquire(13, "second held")
    repeat(10) { frames.acquire(0, "dropped") }
    assertTrue(frames.hasCapacity)
    frames.clear()
    assertEquals(listOf("first held", "second held"), closed.takeLast(2))
  }

  @Test
  fun boundedReaderResumesWhenDroppedFrameIsReleased() {
    val closed = mutableListOf<Int>()
    val frames = RetainedFrames<Int>(3) { closed += it }
    // HWC submits at most two buffers to Wayland. It drops frame 3 before
    // sending it, so a stalled compositor cannot exhaust this reader.
    for (id in 1L..3L) frames.acquire(id, id.toInt())
    assertFalse(frames.hasCapacity)
    assertTrue(closed.isEmpty())
    frames.release(3)
    assertTrue(frames.hasCapacity)
    assertEquals(listOf(3), closed)
    frames.clear()
    assertEquals(listOf(3, 1, 2), closed)
  }

  @Test
  fun reconnectBeforeWindowRequestRetiresLeasesWithoutReleaseEvents() {
    data class Image(var token: Long)
    val closed = mutableListOf<Image>()
    val frames = RetainedFrames<Image>(3) { closed += it }
    val first = Image(1)
    val second = Image(2)
    frames.acquire(1, first)
    frames.acquire(2, second)
    // Input reconnects before HWC: a live host lease must survive.
    frames.retire { it.token == 0L }
    assertTrue(closed.isEmpty())
    // HWC reconnects before the window request. The new bridge cannot route
    // IMAGE_RELEASE, but HWC clears the shared tokens. One reader slot is
    // still available, so the next image callback can reclaim both images.
    first.token = 0
    second.token = 0
    assertTrue(frames.hasCapacity)
    frames.retire { it.token == 0L }
    frames.acquire(3, Image(3))
    frames.acquire(4, Image(4))
    assertEquals(listOf(first, second), closed)
    assertTrue(frames.hasCapacity)
    frames.clear()
    assertEquals(4, closed.size)
  }
}
