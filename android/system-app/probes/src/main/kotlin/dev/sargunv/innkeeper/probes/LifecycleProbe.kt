// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.probes

import android.app.Activity
import android.app.AlarmManager
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.app.job.JobInfo
import android.app.job.JobParameters
import android.app.job.JobScheduler
import android.app.job.JobService
import android.content.BroadcastReceiver
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioTrack
import android.media.session.MediaSession
import android.media.session.PlaybackState
import android.os.Bundle
import android.os.IBinder
import android.os.SystemClock
import android.util.Log
import kotlin.concurrent.thread
import kotlin.math.sin

/**
 * Real alarm-clock and foreground audio APIs for the e2e audio/lifecycle groups, and a JobScheduler
 * job for checking job timing by hand; never shipped.
 */
class LifecycleProbe : Activity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    when (intent.getStringExtra("action")) {
      "media" -> startForegroundService(Intent(this, Media::class.java))
      "stop" -> stopService(Intent(this, Media::class.java))
      "alarm" -> {
        val at = System.currentTimeMillis() + intent.getIntExtra("seconds", 240) * 1000L
        getSharedPreferences("alarm", 0).edit().putLong("at", at).commit()
        schedule(this, at)
      }
      "job" -> {
        // A CPU-only job: a minimum latency in seconds, or a period with --ei period.
        val job = JobInfo.Builder(1, ComponentName(this, Job::class.java))
        val period = intent.getIntExtra("period", 0)
        if (period > 0) job.setPeriodic(period * 1000L)
        else job.setMinimumLatency(intent.getIntExtra("seconds", 20) * 1000L)
        getSystemService(JobScheduler::class.java).schedule(job.build())
        Log.i(TAG, "job scheduled elapsed=${SystemClock.elapsedRealtime()}")
      }
    }
    finish()
  }

  class Job : JobService() {
    override fun onStartJob(params: JobParameters): Boolean {
      Log.i(TAG, "job ran elapsed=${SystemClock.elapsedRealtime()}")
      return false
    }

    override fun onStopJob(params: JobParameters) = false
  }

  class Alarm : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
      val prefs = context.getSharedPreferences("alarm", 0)
      if (intent.action == Intent.ACTION_BOOT_COMPLETED) {
        val at = prefs.getLong("at", 0)
        if (at > System.currentTimeMillis()) schedule(context, at)
      } else {
        Log.i(TAG, "alarm fired at=${System.currentTimeMillis()}")
        prefs.edit().remove("at").apply()
      }
    }
  }

  class Media : Service() {
    @Volatile private var playing = false
    private lateinit var session: MediaSession

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
      super.onCreate()
      session = MediaSession(this, "InnkeeperMediaProbe")
      fun state(playing: Boolean) {
        session.setPlaybackState(
          PlaybackState.Builder()
            .setActions(
              PlaybackState.ACTION_PLAY or
                PlaybackState.ACTION_PAUSE or
                PlaybackState.ACTION_PLAY_PAUSE or
                PlaybackState.ACTION_SKIP_TO_NEXT,
            )
            .setState(
              if (playing) PlaybackState.STATE_PLAYING else PlaybackState.STATE_PAUSED,
              0,
              1f,
            )
            .build(),
        )
      }
      session.setCallback(
        object : MediaSession.Callback() {
          override fun onPlay() {
            Log.i(TAG, "media play")
            state(true)
          }

          override fun onPause() {
            Log.i(TAG, "media pause")
            state(false)
          }

          override fun onSkipToNext() {
            Log.i(TAG, "media next")
          }
        },
      )
      state(true)
      session.isActive = true
      getSystemService(NotificationManager::class.java)
        .createNotificationChannel(
          NotificationChannel("media", "Lifecycle test", NotificationManager.IMPORTANCE_LOW),
        )
      startForeground(
        1,
        Notification.Builder(this, "media")
          .setSmallIcon(android.R.drawable.ic_media_play)
          .setContentTitle("Lifecycle test tone")
          .build(),
      )
      playing = true
      thread(name = "lifecycle-audio") {
        val audio =
          AudioTrack.Builder()
            .setAudioAttributes(
              AudioAttributes.Builder().setUsage(AudioAttributes.USAGE_MEDIA).build(),
            )
            .setAudioFormat(
              AudioFormat.Builder()
                .setSampleRate(48000)
                .setChannelMask(AudioFormat.CHANNEL_OUT_MONO)
                .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                .build(),
            )
            .setBufferSizeInBytes(9600)
            .build()
        val samples =
          ShortArray(4800) { (sin(it * 2 * Math.PI * 440 / 48000) * 12000).toInt().toShort() }
        audio.play()
        Log.i(TAG, "foreground audio started")
        try {
          while (playing) audio.write(samples, 0, samples.size)
        } finally {
          audio.stop()
          audio.release()
        }
      }
    }

    override fun onDestroy() {
      playing = false
      session.release()
      super.onDestroy()
    }
  }

  companion object {
    const val TAG = "InnkeeperLifecycleProbe"

    fun schedule(context: Context, at: Long) {
      val alarm =
        PendingIntent.getBroadcast(
          context,
          1,
          Intent(context, Alarm::class.java),
          PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
      context
        .getSystemService(AlarmManager::class.java)
        .setAlarmClock(AlarmManager.AlarmClockInfo(at, alarm), alarm)
      Log.i(TAG, "alarm scheduled at=$at")
    }
  }
}
