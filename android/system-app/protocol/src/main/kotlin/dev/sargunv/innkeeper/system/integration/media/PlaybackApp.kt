// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.integration.media

/**
 * The app the desktop mixer names on Android's single mixed playback stream, which is the playing
 * app that started most recently. Android mixes every app into that stream, so this is a label and
 * gives no per-app volume. [PlaybackClient.startedAt] is an arbitrary comparable clock, such as
 * AudioTrack's player-interface id, which grows as players start.
 */
data class PlaybackClient(val uid: Int, val startedAt: Long, val playing: Boolean)

/**
 * The most recently started client that is playing, or null when none is. A later start wins a tie,
 * and a higher uid breaks one that remains, so the choice does not flap between equals.
 */
fun playingApp(clients: List<PlaybackClient>): PlaybackClient? =
  clients.filter { it.playing }.maxWithOrNull(compareBy({ it.startedAt }, { it.uid }))

/**
 * The package the mixer should name for [uid], or null for a uid with no package (system playback)
 * or one of our own. When several packages share the uid, this takes the first, which is the
 * package Android reports as owning the uid.
 */
fun playbackPackage(uid: Int, packagesForUid: (Int) -> List<String>?, ownPackage: String): String? {
  val pkg = packagesForUid(uid)?.firstOrNull() ?: return null
  if (pkg.isEmpty() || pkg == ownPackage) return null
  return pkg
}
