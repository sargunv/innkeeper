// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.host

import android.net.LocalSocket
import android.net.LocalSocketAddress
import android.util.Log
import dev.linuxapk.system.integration.Blob
import dev.linuxapk.system.integration.Wire
import java.io.FileDescriptor
import java.io.IOException
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.concurrent.CompletableFuture
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger

/**
 * One connection to a host bridge service (docs/bridge.md): HELLO for [service], then blocking
 * [call]s from any thread, whose replies may carry fds, and events to [onEvent] on the reader
 * thread. Reconnects with backoff; [onConnected] runs on a thread of its own after each HELLO.
 */
class HostChannel(
  private val service: Int,
  private val name: String,
  private val onConnected: () -> Unit = {},
  private val onDisconnected: () -> Unit = {},
  private val onEvent: (type: Int, body: ByteBuffer, fds: Array<FileDescriptor>) -> Unit =
    { _, _, _ ->
    },
  private val path: String = SOCKET,
) {
  /** A reply: status 0 or -errno, the body, and the fds it carried (the caller closes them). */
  class Reply(val status: Int, val body: ByteBuffer, val fds: Array<FileDescriptor>) {
    val ok
      get() = status == 0

    fun closeFds() = fds.forEach(Blob::close)
  }

  @Volatile private var running = false
  @Volatile private var socket: LocalSocket? = null
  @Volatile
  var connected = false
    private set

  private var thread: Thread? = null
  private val tags = AtomicInteger(2)
  private val pending = ConcurrentHashMap<Int, CompletableFuture<Reply>>()
  private val writeLock = Any()

  fun start() {
    check(thread == null)
    running = true
    thread = Thread(::run, "linuxapk-host-$service").apply { start() }
  }

  fun stop() {
    running = false
    try {
      socket?.shutdownInput()
    } catch (_: IOException) {}
    thread?.interrupt()
    thread = null
  }

  /**
   * Sends request [type] and waits for its reply (null: not connected, failed or timed out). [fd],
   * if given, goes along and stays the caller's.
   */
  fun call(
    type: Int,
    fd: FileDescriptor? = null,
    timeoutMs: Long = 10_000,
    body: Wire.Writer.() -> Unit = {},
  ): Reply? {
    val s = socket ?: return null
    if (!connected) return null
    val tag = tags.getAndIncrement() and 0x7fffffff
    val msg = Wire.message(type, tag).apply(body).bytes()
    if (msg.size > Wire.MAX_MSG) return null
    val f = CompletableFuture<Reply>()
    pending[tag] = f
    return try {
      write(s, msg, fd)
      f.get(timeoutMs, TimeUnit.MILLISECONDS)
    } catch (e: Exception) {
      Log.w(TAG, "request 0x${type.toString(16)}: $e")
      null
    } finally {
      pending.remove(tag)
    }
  }

  private fun write(s: LocalSocket, msg: ByteArray, fd: FileDescriptor?) {
    synchronized(writeLock) {
      s.setFileDescriptorsForSend(if (fd != null) arrayOf(fd) else null)
      try {
        s.outputStream.write(msg)
      } finally {
        s.setFileDescriptorsForSend(null)
      }
    }
  }

  private fun run() {
    var backoff = 500L
    while (running) {
      val s = LocalSocket(LocalSocket.SOCKET_SEQPACKET)
      try {
        s.connect(LocalSocketAddress(path, LocalSocketAddress.Namespace.FILESYSTEM))
        socket = s
        session(s)
        backoff = 500L
      } catch (e: IOException) {
        if (running && backoff == 500L) Log.i(TAG, "bridge service $service: ${e.message}")
      } catch (e: Exception) {
        Log.e(TAG, "bridge service $service failed", e)
      } finally {
        val was = connected
        connected = false
        socket = null
        pending.values.forEach { it.cancel(false) }
        try {
          s.close()
        } catch (_: IOException) {}
        if (was) {
          Log.i(TAG, "disconnected from bridge service $service")
          onDisconnected()
        }
      }
      if (!running) break
      try {
        Thread.sleep(backoff)
      } catch (_: InterruptedException) {}
      backoff = minOf(backoff * 2, 10_000L)
    }
  }

  private fun session(s: LocalSocket) {
    val buf = ByteArray(Wire.MAX_MSG + 1)
    write(
      s,
      Wire.message(Wire.HELLO, 1).u16(1).u16(0).u32(service).name(name).bytes(),
      null,
    )
    val input = s.inputStream
    while (running) {
      val n = input.read(buf)
      if (n <= 0) return
      var fds: Array<FileDescriptor> = s.ancillaryFileDescriptors ?: emptyArray()
      try {
        if (n < Wire.HDR || n > Wire.MAX_MSG) throw IOException("bad packet ($n bytes)")
        val r = ByteBuffer.wrap(buf, 0, n).order(ByteOrder.LITTLE_ENDIAN)
        val type = r.short.toInt() and 0xffff
        r.short
        val tag = r.int
        if (type == Wire.REPLY) {
          val status = r.int
          if (tag == 1) {
            if (status != 0) throw IOException("bridge refused service $service: $status")
            connected = true
            Log.i(TAG, "connected to bridge service $service")
            // On its own thread: it may make calls, whose replies this thread reads.
            Thread(onConnected, "linuxapk-host-$service-up").start()
            continue
          }
          val body =
            ByteBuffer.wrap(buf.copyOfRange(r.position(), n)).order(ByteOrder.LITTLE_ENDIAN)
          val f = pending.remove(tag)
          if (f != null && f.complete(Reply(status, body, fds))) fds = emptyArray()
        } else if (type in 0x4000..0x7fff && connected) {
          val body =
            ByteBuffer.wrap(buf.copyOfRange(r.position(), n)).order(ByteOrder.LITTLE_ENDIAN)
          try {
            onEvent(type, body, fds)
          } catch (e: Exception) {
            Log.w(TAG, "event 0x${type.toString(16)}", e)
          }
        }
      } finally {
        fds.forEach(Blob::close)
      }
    }
  }

  companion object {
    const val TAG = "LinuxApkHost"
    const val SOCKET = "/dev/socket/linuxapk/bridge.sock"
    const val SERVICE_DOCUMENTS = 4
    const val SERVICE_APPS = 5
  }
}
