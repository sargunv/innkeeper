// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.probes

import android.app.Activity
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.AudioTimestamp
import android.media.AudioTrack
import android.media.MediaRecorder
import android.os.Bundle
import android.util.Log
import android.view.KeyEvent
import android.view.View
import kotlin.concurrent.thread
import kotlin.math.sin

/** Manual input and frame benchmarks; e2e reuses the audio marker to prove capture. */
class BenchProbe : Activity() {
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
    registerReceiver(
      control,
      IntentFilter("dev.sargunv.innkeeper.BENCH_BEGIN"),
      Context.RECEIVER_EXPORTED,
    )
  }

  override fun onResume() {
    super.onResume()
    Log.i("InnkeeperBench", "BENCH resumed display=${display?.displayId}")
  }

  override fun onStop() {
    Log.i("InnkeeperBench", "BENCH stopped display=${display?.displayId}")
    super.onStop()
  }

  private fun begin() {
    // The runner starts workers after the desktop window has mapped, so moving
    // the activity off display 0 cannot create duplicate measurement workers.
    if (started) return
    started = true
    if (intent.getStringExtra("mode") == "audio") thread(name = "bench-audio") { audio() }
  }

  override fun onDestroy() {
    unregisterReceiver(control)
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
        "InnkeeperBench",
        "BENCH receive ns=${System.nanoTime()} color=${if (blue) 16711680 else 255}",
      )
      invalidate()
      return true
    }

    override fun onDraw(canvas: Canvas) {
      if (mode == "input")
        Log.i(
          "InnkeeperBench",
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
        Log.i("InnkeeperBench", "BENCH ready mode=$mode")
        ready = true
      }
      frame++
      if (mode == "scroll" || mode == "game") postInvalidateOnAnimation()
    }
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
        "InnkeeperBench",
        "BENCH audio output_ns=${latency ?: -1} roundtrip_ns=${if (detected > at) detected - at else -1} capture_peak=$peak",
      )
    } catch (e: Exception) {
      Log.e("InnkeeperBench", "BENCH error audio", e)
    } finally {
      track.release()
      record.release()
    }
  }
}
