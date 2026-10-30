// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.probes

import android.app.Activity
import android.app.Service
import android.content.BroadcastReceiver
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.ServiceConnection
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.AudioTimestamp
import android.media.AudioTrack
import android.media.MediaRecorder
import android.os.Binder
import android.os.Bundle
import android.os.IBinder
import android.os.Parcel
import android.util.Log
import android.view.KeyEvent
import android.view.View
import java.util.concurrent.CountDownLatch
import kotlin.concurrent.thread
import kotlin.math.sin

/** Manual benchmarks; e2e also reuses the audio marker to prove capture. */
class BenchProbe : Activity() {
  private var connection: ServiceConnection? = null
  private var started = false
  private val control =
    object : BroadcastReceiver() {
      override fun onReceive(context: Context, intent: Intent) {
        begin()
      }
    }

  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    val mode = intent.getStringExtra("mode") ?: "input"
    setContentView(ProbeView(this, mode))
    registerReceiver(control, IntentFilter("dev.linuxapk.BENCH_BEGIN"), Context.RECEIVER_EXPORTED)
  }

  override fun onResume() {
    super.onResume()
    Log.i("LinuxApkBench", "BENCH resumed display=${display?.displayId}")
  }

  override fun onStop() {
    Log.i("LinuxApkBench", "BENCH stopped display=${display?.displayId}")
    super.onStop()
  }

  private fun begin() {
    // The runner starts workers after the desktop window has mapped, so moving
    // the activity off display 0 cannot create duplicate measurement workers.
    if (started) return
    started = true
    when (intent.getStringExtra("mode")) {
      "binder" -> binder()
      "audio" -> thread(name = "bench-audio") { audio() }
    }
  }

  override fun onDestroy() {
    unregisterReceiver(control)
    connection?.let { unbindService(it) }
    super.onDestroy()
  }

  private class ProbeView(context: Context, val mode: String) : View(context) {
    val paint = Paint()
    var blue = false
    var frame = 0
    var ready = false

    init {
      isFocusableInTouchMode = true
      requestFocus()
    }

    override fun onKeyDown(code: Int, event: KeyEvent): Boolean {
      if (code != KeyEvent.KEYCODE_SPACE) return super.onKeyDown(code, event)
      blue = !blue
      Log.i(
        "LinuxApkBench",
        "BENCH receive ns=${System.nanoTime()} color=${if (blue) 16711680 else 255}",
      )
      invalidate()
      return true
    }

    override fun onDraw(canvas: Canvas) {
      if (mode == "input")
        Log.i(
          "LinuxApkBench",
          "BENCH draw ns=${System.nanoTime()} color=${if (blue) 16711680 else 255}",
        )
      canvas.drawColor(if (blue) Color.BLUE else Color.RED)
      if (mode == "scroll") {
        // A real scrolling list, with the same fixed workload on each run.
        paint.color = Color.WHITE
        paint.textSize = 32f
        val offset = (frame * 7) % 60
        for (row in 0..40) canvas.drawText(
          "Benchmark row ${row + frame / 9}",
          20f,
          (row * 60 - offset).toFloat(),
          paint,
        )
      } else if (mode == "game") {
        for (i in 0..199) {
          paint.color = Color.rgb(i, 255 - i, 180)
          val x = ((i * 73 + frame * 5) % width.coerceAtLeast(1)).toFloat()
          val y = (height / 2 + sin((frame + i) * 0.04) * height * 0.4).toFloat()
          canvas.drawCircle(x, y, 12f, paint)
        }
      }
      if (!ready) {
        Log.i("LinuxApkBench", "BENCH ready mode=$mode")
        ready = true
      }
      frame++
      if (mode == "scroll" || mode == "game") postInvalidateOnAnimation()
    }
  }

  private fun binder() {
    val c =
      object : ServiceConnection {
        override fun onServiceDisconnected(name: ComponentName) {}

        override fun onServiceConnected(name: ComponentName, remote: IBinder) {
          thread(name = "bench-binder") {
            try {
              check(remote !is Binder) { "benchmark requires a remote process" }
              for (workers in listOf(1, 8)) {
                val start = CountDownLatch(1)
                val done = CountDownLatch(workers)
                val samples = Array(workers) { LongArray(500) }
                val failures = java.util.concurrent.atomic.AtomicInteger()
                for (w in 0 until workers) thread(name = "binder-$w") {
                  start.await()
                  try {
                    repeat(500) { i ->
                      val data = Parcel.obtain()
                      val reply = Parcel.obtain()
                      try {
                        data.writeInt(i)
                        val at = System.nanoTime()
                        check(remote.transact(IBinder.FIRST_CALL_TRANSACTION, data, reply, 0))
                        check(reply.readInt() == i)
                        samples[w][i] = System.nanoTime() - at
                      } finally {
                        data.recycle()
                        reply.recycle()
                      }
                    }
                  } catch (e: Exception) {
                    failures.incrementAndGet()
                    Log.e("LinuxApkBench", "binder", e)
                  } finally {
                    done.countDown()
                  }
                }
                val at = System.nanoTime()
                start.countDown()
                done.await()
                val elapsed = System.nanoTime() - at
                check(failures.get() == 0)
                // Keep each log line below logcat's payload limit.
                for (w in samples.indices) for (chunk in samples[w].toList().chunked(100)) {
                  Log.i(
                    "LinuxApkBench",
                    "BENCH binder_sample workers=$workers ns=${chunk.joinToString(",")}",
                  )
                }
                Log.i(
                  "LinuxApkBench",
                  "BENCH binder workers=$workers count=${workers * 500} elapsed_ns=$elapsed",
                )
              }
            } catch (e: Exception) {
              Log.e("LinuxApkBench", "BENCH error binder", e)
            }
          }
        }
      }
    connection = c
    check(bindService(Intent(this, BenchService::class.java), c, Context.BIND_AUTO_CREATE))
  }

  private fun audio() {
    val rate = 48000
    val track =
      AudioTrack.Builder()
        .setAudioAttributes(AudioAttributes.Builder().setUsage(AudioAttributes.USAGE_MEDIA).build())
        .setAudioFormat(
          AudioFormat.Builder()
            .setSampleRate(rate)
            .setChannelMask(AudioFormat.CHANNEL_OUT_MONO)
            .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
            .build(),
        )
        .setBufferSizeInBytes(9600)
        .build()
    val record =
      AudioRecord(
        MediaRecorder.AudioSource.MIC,
        rate,
        AudioFormat.CHANNEL_IN_MONO,
        AudioFormat.ENCODING_PCM_16BIT,
        9600,
      )
    val input = ShortArray(480)
    var detected = 0L
    var peak = 0
    var state = 1
    val marker =
      ShortArray(127) {
        state = state * 1664525 + 1013904223
        if (state < 0) 16000 else -16000
      }
    try {
      record.startRecording()
      val capture =
        thread(name = "bench-capture") {
          val end = System.nanoTime() + 3_000_000_000L
          val ring = ShortArray(127)
          var index = 0
          while (System.nanoTime() < end) {
            val n = record.read(input, 0, input.size, AudioRecord.READ_NON_BLOCKING)
            val at = System.nanoTime()
            for (i in 0 until n.coerceAtLeast(0)) {
              peak = maxOf(peak, kotlin.math.abs(input[i].toInt()))
              ring[index++ % 127] = input[i]
              if (index < 127 || detected != 0L) continue
              var dot = 0.0
              var energy = 0.0
              for (j in marker.indices) {
                val v = ring[(index + j) % 127].toDouble()
                dot += v * (if (marker[j] > 0) 1 else -1)
                energy += v * v
              }
              if (energy > 1e8 && dot > 0 && dot * dot > 0.9 * 127 * energy) detected = at
            }
            Thread.sleep(2)
          }
        }
      val samples = ShortArray(4800)
      marker.copyInto(samples)
      val timestamp = AudioTimestamp()
      val at = System.nanoTime()
      track.play()
      var latency: Long? = null
      repeat(20) {
        track.write(samples, 0, samples.size)
        if (track.getTimestamp(timestamp)) {
          latency = timestamp.nanoTime - at - timestamp.framePosition * 1_000_000_000L / rate
        }
      }
      capture.join()
      Log.i(
        "LinuxApkBench",
        "BENCH audio output_ns=${latency ?: -1} roundtrip_ns=${if (detected > at) detected - at else -1} capture_peak=$peak",
      )
    } catch (e: Exception) {
      Log.e("LinuxApkBench", "BENCH error audio", e)
    } finally {
      track.release()
      record.release()
    }
  }
}

class BenchService : Service() {
  private val echo =
    object : Binder() {
      override fun onTransact(code: Int, data: Parcel, reply: Parcel?, flags: Int): Boolean {
        if (code != IBinder.FIRST_CALL_TRANSACTION)
          return super.onTransact(code, data, reply, flags)
        reply!!.writeInt(data.readInt())
        return true
      }
    }

  override fun onBind(intent: Intent): IBinder = echo
}
