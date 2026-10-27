// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.input

import android.net.LocalSocket
import android.net.LocalSocketAddress
import android.os.Process
import android.util.Log
import java.io.BufferedReader
import java.io.IOException

/**
 * Client of the composer's input stream. Reads lines on its own thread, translates them and hands
 * the Android events to [sink]. Reconnects (with backoff) when the composer is absent or restarts,
 * releasing anything held first.
 */
class InputBridge(
  private val socketPath: String,
  private val newTranslator: () -> InputTranslator,
  private val sink: (OutEvent) -> Unit,
) {
  @Volatile private var running = false
  @Volatile private var socket: LocalSocket? = null
  private var thread: Thread? = null

  fun start() {
    check(thread == null)
    running = true
    thread = Thread(::run, "linuxapk-input").apply { start() }
  }

  fun stop() {
    running = false
    try {
      socket?.shutdownInput()
    } catch (_: IOException) {}
    thread?.interrupt()
    thread?.join(2000)
    thread = null
  }

  private fun run() {
    // Input latency matters; negative nice values need RLIMIT_NICE, which the rootless session
    // may not grant, so take what we can get.
    for (prio in
      intArrayOf(Process.THREAD_PRIORITY_URGENT_DISPLAY, Process.THREAD_PRIORITY_DISPLAY)) {
      try {
        Process.setThreadPriority(prio)
        break
      } catch (_: SecurityException) {}
    }
    var backoff = MIN_BACKOFF_MS
    var lastError: String? = null
    while (running) {
      val translator = newTranslator()
      try {
        LocalSocket().use { s ->
          s.connect(LocalSocketAddress(socketPath, LocalSocketAddress.Namespace.FILESYSTEM))
          socket = s
          Log.i(TAG, "connected to $socketPath")
          lastError = null
          backoff = MIN_BACKOFF_MS
          pump(s.inputStream.bufferedReader(Charsets.US_ASCII), translator, sink) { running }
          Log.i(TAG, "input stream closed")
        }
      } catch (e: IOException) {
        val msg = e.message ?: e.toString()
        if (msg != lastError) Log.i(TAG, "$socketPath: $msg (retrying)")
        lastError = msg
      } finally {
        socket = null
        translator.reset().forEach(sink)
      }
      if (!running) break
      try {
        Thread.sleep(backoff)
      } catch (_: InterruptedException) {
        break
      }
      backoff = (backoff * 2).coerceAtMost(MAX_BACKOFF_MS)
    }
  }

  companion object {
    const val TAG = "LinuxApkInput"

    /** Translates lines from [reader] until EOF or until [keepGoing] returns false. */
    fun pump(
      reader: BufferedReader,
      translator: InputTranslator,
      sink: (OutEvent) -> Unit,
      keepGoing: () -> Boolean = { true },
    ) {
      while (keepGoing()) {
        val line = reader.readLine() ?: break
        val event = HostEvent.parse(line)
        if (event == null) {
          Log.w(TAG, "ignoring line: $line")
          continue
        }
        if (event is HostEvent.Hello && event.version != HostEvent.PROTOCOL_VERSION) {
          Log.w(TAG, "protocol version ${event.version}, expected ${HostEvent.PROTOCOL_VERSION}")
        }
        translator.handle(event).forEach(sink)
      }
    }

    private const val MIN_BACKOFF_MS = 250L
    private const val MAX_BACKOFF_MS = 5000L
  }
}
