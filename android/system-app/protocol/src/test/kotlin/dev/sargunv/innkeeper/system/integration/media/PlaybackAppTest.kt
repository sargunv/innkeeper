// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.integration.media

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull

class PlaybackAppTest {
  private fun client(uid: Int, startedAt: Long, playing: Boolean) =
    PlaybackClient(uid, startedAt, playing)

  @Test
  fun newestPlayingAppWinsAndStoppedAppsAreIgnored() {
    val older = client(10001, 10, true)
    val newer = client(10002, 20, true)
    val newestStopped = client(10003, 30, false)
    assertEquals(newer, playingApp(listOf(older, newestStopped, newer)))
    assertEquals(older, playingApp(listOf(older, newestStopped)))
    assertNull(playingApp(listOf(newestStopped)))
    assertNull(playingApp(emptyList()))
  }

  @Test
  fun equalStartsBreakTheTieByUidSoTheChoiceDoesNotFlap() {
    val low = client(10001, 10, true)
    val high = client(10086, 10, true)
    assertEquals(high, playingApp(listOf(low, high)))
    assertEquals(high, playingApp(listOf(high, low)))
  }

  @Test
  fun packageResolutionSkipsSystemAndOurOwnUid() {
    val packages =
      mapOf(10001 to listOf("org.example.player"), 10002 to listOf("dev.sargunv.innkeeper.system"))
    val lookup = { uid: Int -> packages[uid] }
    assertEquals(
      "org.example.player",
      playbackPackage(10001, lookup, "dev.sargunv.innkeeper.system"),
    )
    assertEquals(
      "org.example.player",
      playbackPackage(
        10001,
        { listOf("org.example.player", "org.example.player.shared") },
        "dev.sargunv.innkeeper.system",
      ),
    )
    assertNull(playbackPackage(1000, lookup, "dev.sargunv.innkeeper.system"))
    assertNull(playbackPackage(10002, lookup, "dev.sargunv.innkeeper.system"))
    assertNull(playbackPackage(10003, { emptyList() }, "dev.sargunv.innkeeper.system"))
  }
}
