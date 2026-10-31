// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.input

/**
 * Maps the compositor's u32 millisecond timestamps onto Android's uptime clock.
 *
 * Keeps the host's spacing between events (VelocityTracker uses it for flings) but never runs ahead
 * of [now], never falls more than [maxLagMs] behind it, and never goes backwards. The u32 clock
 * wraps after 49 days; deltas are taken modulo 2^32.
 */
class EventClock(private val now: () -> Long, private val maxLagMs: Long = 100) {
  private var lastHost = -1L
  private var last = Long.MIN_VALUE

  /** Android time for a host timestamp. */
  fun map(hostMs: Long): Long {
    val n = now()
    var t =
      if (lastHost < 0) n else last + (((hostMs - lastHost) shl 32) shr 32) // signed u32 delta
    t = t.coerceIn(n - maxLagMs, n)
    lastHost = hostMs
    return monotonic(t)
  }

  /** Android time for an event without a host timestamp. */
  fun untimed(): Long = monotonic(maxOf(last, now() - maxLagMs).coerceAtMost(now()))

  /** The last time handed out (or now, if none yet). */
  fun current(): Long = if (last == Long.MIN_VALUE) untimed() else last

  private fun monotonic(t: Long): Long {
    last = maxOf(last, t)
    return last
  }
}
