// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.integration

import android.net.LocalSocket
import android.net.LocalSocketAddress
import android.util.Log
import java.io.FileDescriptor
import java.io.IOException
import java.nio.ByteBuffer
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicInteger

/**
 * Client of the host bridge's system service (docs/bridge.md, "System service"). Connects to
 * [path], says HELLO and START, then reads events on its own thread and hands them to [listener].
 * Reconnects with backoff when the bridge is absent or restarts.
 *
 * Requests can be sent from any thread; each gets a reply, matched by tag, which goes to its
 * optional callback on the reader thread.
 */
class BridgeClient(
  private val path: String,
  private val features: Int,
  private val listener: Listener,
) {
  interface Listener {
    /** Connected and started; [features] is what the bridge serves. Reader thread. */
    fun onConnected(features: Int)

    /** One event. [fds] are closed after this returns. Reader thread. */
    fun onEvent(type: Int, body: ByteBuffer, fds: Array<FileDescriptor>)

    fun onDisconnected() {}
  }

  @Volatile private var running = false
  @Volatile private var socket: LocalSocket? = null
  @Volatile
  var connected = false
    private set

  private var thread: Thread? = null
  private val tags = AtomicInteger(1)
  private val pending = ConcurrentHashMap<Int, (Int, ByteBuffer) -> Unit>()
  private val writeLock = Any()

  fun start() {
    check(thread == null)
    running = true
    thread = Thread(::run, "linuxapk-bridge").apply { start() }
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

  /**
   * Sends request [type] with [body]; [blob], if given, goes along as a sealed memfd. Returns false
   * if not connected or the write failed.
   */
  fun request(
    type: Int,
    body: Wire.Writer.() -> Unit,
    blob: ByteArray? = null,
    onReply: ((Int, ByteBuffer) -> Unit)? = null,
  ): Boolean {
    val s = socket ?: return false
    if (!connected) return false
    val tag = tags.getAndIncrement() and 0x7fffffff
    val msg = Wire.message(type, tag).apply(body).bytes()
    if (msg.size > Wire.MAX_MSG) return false
    val fd = blob?.let { Blob.create(it) }
    if (onReply != null) pending[tag] = onReply
    return try {
      write(s, msg, fd)
      true
    } catch (e: IOException) {
      pending.remove(tag)
      Log.w(TAG, "send failed: ${e.message}")
      false
    } finally {
      Blob.close(fd)
    }
  }

  private fun write(s: LocalSocket, msg: ByteArray, fd: FileDescriptor?) {
    synchronized(writeLock) {
      // LocalSocket keeps outbound fds until replaced, so set them for every write.
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
        if (running && backoff == 500L) Log.i(TAG, "bridge at $path: ${e.message}")
      } catch (e: Exception) {
        Log.e(TAG, "bridge session failed", e)
      } finally {
        val was = connected
        connected = false
        socket = null
        pending.clear()
        try {
          s.close()
        } catch (_: IOException) {}
        if (was) {
          Log.i(TAG, "disconnected from the bridge")
          listener.onDisconnected()
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
    val input = s.inputStream
    // HELLO, then START; both answered before any event matters.
    write(
      s,
      Wire.message(Wire.HELLO, 0)
        .u16(1)
        .u16(0)
        .u32(Wire.SERVICE_SYSTEM)
        .name("Linux.apk System")
        .bytes(),
      null,
    )
    write(s, Wire.message(Wire.START, 1).u32(features).bytes(), null)
    var started = false
    // Events sent while START is handled (the first SETTINGS, the current clip) arrive before its
    // reply; hold them until then.
    val early = ArrayList<Pair<ByteArray, Array<FileDescriptor>>>()
    while (running) {
      val n = input.read(buf)
      if (n <= 0) return
      var fds = s.ancillaryFileDescriptors ?: emptyArray()
      try {
        if (n < Wire.HDR || n > Wire.MAX_MSG) throw IOException("bad packet ($n bytes)")
        val r = Wire.reader(buf, 0, n)
        val type = r.short.toInt() and 0xffff
        r.short
        val tag = r.int
        if (type == Wire.REPLY) {
          val status = r.int
          if (!started) {
            if (status != 0) throw IOException("bridge refused (tag $tag: $status)")
            if (tag == 1) {
              started = true
              connected = true
              val have = if (r.remaining() >= 4) r.int else 0
              Log.i(TAG, "connected to the bridge (features 0x${have.toString(16)})")
              listener.onConnected(have)
              for ((b, f) in early) {
                try {
                  dispatch(b, b.size, f)
                } finally {
                  for (fd in f) Blob.close(fd)
                }
              }
              early.clear()
            }
            continue
          }
          val cb = pending.remove(tag)
          if (cb != null) cb(status, r.slice().order(java.nio.ByteOrder.LITTLE_ENDIAN))
          else if (status != 0) Log.w(TAG, "request $tag failed: $status")
        } else if (type in 0x4000..0x7fff) {
          if (started) {
            dispatch(buf, n, fds)
          } else if (early.size < 16) {
            early += buf.copyOf(n) to fds
            fds = emptyArray() // closed once dispatched
          }
        }
      } finally {
        for (fd in fds) Blob.close(fd)
      }
    }
    for ((_, f) in early) for (fd in f) Blob.close(fd)
  }

  private fun dispatch(b: ByteArray, n: Int, fds: Array<FileDescriptor>) {
    val r = Wire.reader(b, 0, n)
    val type = r.short.toInt() and 0xffff
    r.short
    r.int
    try {
      listener.onEvent(type, r.slice().order(java.nio.ByteOrder.LITTLE_ENDIAN), fds)
    } catch (e: Exception) {
      Log.w(TAG, "event 0x${type.toString(16)}", e)
    }
  }

  companion object {
    const val TAG = "LinuxApkBridge"
    const val SOCKET = "/dev/socket/linuxapk/bridge.sock"
  }
}
