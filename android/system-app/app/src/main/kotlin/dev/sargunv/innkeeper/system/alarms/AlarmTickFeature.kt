// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.alarms

import android.app.AlarmManager
import android.content.Context
import android.os.Handler
import android.os.HandlerThread
import android.os.SystemClock
import dev.sargunv.innkeeper.system.Feature

/**
 * Delivers due inexact alarms within a minute, as a busy phone does.
 *
 * AlarmManager holds an inexact alarm until the end of its window (75% of its delay for
 * JobScheduler's `*job.delay*` alarm, which is what runs a job once its minimum latency passes)
 * unless an earlier delivery carries it. On a phone other apps' alarms do that; in an idle session
 * the only alarms within minutes are TIME_TICK and power stats, and those are exact (standalone),
 * so they carry nothing. A ready 15-minute periodic job then ran 11 minutes late (#54). This alarm
 * is inexact, so each delivery also delivers every non-wakeup alarm already due.
 */
class AlarmTickFeature(context: Context) : Feature {
  override val name = "alarm tick"

  private val alarms = context.getSystemService(AlarmManager::class.java)
  private val thread = HandlerThread("alarm-tick")
  private lateinit var handler: Handler
  private val tick = AlarmManager.OnAlarmListener { schedule() }

  override fun start() {
    thread.start()
    handler = Handler(thread.looper)
    schedule()
  }

  private fun schedule() {
    // 30 s, plus a 30 s window (AlarmManager's minimum for us is 75% of the delay).
    alarms.setWindow(
      AlarmManager.ELAPSED_REALTIME,
      SystemClock.elapsedRealtime() + 30_000,
      30_000,
      "innkeeper-alarm-tick",
      tick,
      handler,
    )
  }
}
