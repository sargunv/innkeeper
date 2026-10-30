// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.probes

import android.app.Activity
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import android.os.Bundle
import android.os.SystemClock
import android.util.Log
import android.widget.TextView
import java.io.File
import java.nio.ByteBuffer
import java.nio.ByteOrder
import kotlin.concurrent.thread

/** Manual sustained capture proof; the caller supplies a private virtual source. */
class AudioCaptureProbe : Activity() {
  private var started = false

  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    setContentView(TextView(this).apply { text = "Recording the private test source" })
  }

  override fun onWindowFocusChanged(hasFocus: Boolean) {
    super.onWindowFocusChanged(hasFocus)
    if (!hasFocus || started) return
    started = true
    thread(name = "capture-proof") {
      val seconds = intent.getIntExtra("seconds", 32).coerceIn(1, 40)
      val recorder =
        AudioRecord.Builder()
          .setAudioSource(MediaRecorder.AudioSource.MIC)
          .setAudioFormat(
            AudioFormat.Builder()
              .setSampleRate(48000)
              .setChannelMask(AudioFormat.CHANNEL_IN_MONO)
              .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
              .build(),
          )
          .setBufferSizeInBytes(19200)
          .build()
      try {
        recorder.startRecording()
        File(filesDir, "capture.raw").outputStream().use { output ->
          val samples = ShortArray(480)
          val bytes = ByteBuffer.allocate(960).order(ByteOrder.LITTLE_ENDIAN)
          var remaining = seconds * 48000
          while (remaining > 0) {
            val n = recorder.read(samples, 0, minOf(samples.size, remaining))
            check(n > 0) { "AudioRecord returned $n" }
            bytes.clear()
            for (i in 0 until n) bytes.putShort(samples[i])
            output.write(bytes.array(), 0, n * 2)
            remaining -= n
          }
        }
        Log.i(
          "CaptureProof",
          "complete frames=${seconds * 48000} at=${SystemClock.elapsedRealtimeNanos()}",
        )
      } catch (e: Exception) {
        Log.e("CaptureProof", "failed", e)
      } finally {
        recorder.release()
      }
    }
  }
}
