// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.probes

import android.app.Activity
import android.app.AlarmManager
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioTrack
import android.os.Bundle
import android.os.IBinder
import android.util.Log
import java.net.HttpURLConnection
import java.net.URI
import kotlin.concurrent.thread
import kotlin.math.sin

/** Real alarm-clock and foreground audio APIs for tests/lifecycle.sh; never shipped. */
class LifecycleProbe : Activity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    when (intent.getStringExtra("action")) {
      "network" ->
        thread(name = "lifecycle-network") {
          try {
            val connection =
              URI("https://example.com/").toURL().openConnection() as HttpURLConnection
            connection.connectTimeout = 10000
            connection.readTimeout = 10000
            try {
              val bytes = connection.inputStream.use { it.read(ByteArray(512)) }
              Log.i(TAG, "network status=${connection.responseCode} bytes=$bytes")
            } finally {
              connection.disconnect()
            }
          } catch (e: Exception) {
            Log.e(TAG, "network failed", e)
          }
        }
      "media" -> startForegroundService(Intent(this, Media::class.java))
      "stop" -> stopService(Intent(this, Media::class.java))
      "alarm" -> {
        val at = System.currentTimeMillis() + intent.getIntExtra("seconds", 240) * 1000L
        getSharedPreferences("alarm", 0).edit().putLong("at", at).commit()
        schedule(this, at)
      }
    }
    finish()
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

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
      super.onCreate()
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
      super.onDestroy()
    }
  }

  companion object {
    const val TAG = "LinuxApkLifecycleProbe"

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
