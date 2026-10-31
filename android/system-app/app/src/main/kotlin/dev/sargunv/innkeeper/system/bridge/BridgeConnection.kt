// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.bridge

import android.net.LocalSocket
import android.net.LocalSocketAddress
import android.os.Process
import android.util.Log
import java.io.FileDescriptor
import java.io.IOException
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.concurrent.CompletableFuture
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger

/**
 * One connection to a host bridge service (docs/bridge-protocol.md): HELLO for [service], then
 * requests from any thread, each answered by a reply that may carry fds, and events for [listener]
 * on the reader thread. Reconnects with backoff.
 *
 * [latencySensitive]: the reader thread runs at display priority (host input).
 */
class BridgeConnection(
  private val service: Int,
  private val name: String,
  private val listener: Listener,
  private val latencySensitive: Boolean = false,
  private val path: String = SOCKET,
) {
  interface Listener {
    /** After each HELLO, on a thread of its own: it may make blocking [call]s. */
    fun onConnected() {}

    /** The connection ended; reader thread. */
    fun onDisconnected() {}

    /** One event; [fds] are closed when this returns. Reader thread. */
    fun onEvent(type: Int, body: ByteBuffer, fds: Array<FileDescriptor>) {}
  }

  /** A reply: status 0 or -errno, the body, and the fds it carried. */
  class Reply(val status: Int, val body: ByteBuffer, val fds: Array<FileDescriptor>) {
    val ok
      get() = status == 0

    fun closeFds() = fds.forEach(Blob::close)
  }

  @Volatile private var socket: LocalSocket? = null
  @Volatile
  var connected = false
    private set

  private var thread: Thread? = null
  private val tags = AtomicInteger(HELLO_TAG + 1)
  // Reply handlers by tag; each owns the reply's fds.
  private val pending = ConcurrentHashMap<Int, (Reply) -> Unit>()
  private val writeLock = Any()

  fun start() {
    check(thread == null)
    thread = Thread(::run, "innkeeper-bridge-$service").apply { start() }
  }

  /**
   * Sends request [type] and waits for its reply (null: not connected, failed or timed out). The
   * caller closes the reply's fds. [fd], if given, goes along and stays the caller's.
   */
  fun call(
    type: Int,
    fd: FileDescriptor? = null,
    timeoutMs: Long = 10_000,
    body: Wire.Writer.() -> Unit = {},
  ): Reply? {
    val f = CompletableFuture<Reply>()
    if (!write(type, fd, body) { r -> if (!f.complete(r)) r.closeFds() }) return null
    return try {
      f.get(timeoutMs, TimeUnit.MILLISECONDS)
    } catch (e: Exception) {
      f.cancel(false) // a late reply closes its fds
      Log.w(TAG, "service $service request 0x${type.toString(16)}: $e")
      null
    }
  }

  /**
   * Sends request [type] without waiting. [blob], if given, goes along as a sealed memfd.
   * [onReply], if given, gets the reply on the reader thread; its fds are closed after. Returns
   * false if not connected or the write failed.
   */
  fun send(
    type: Int,
    blob: ByteArray? = null,
    onReply: ((Reply) -> Unit)? = null,
    body: Wire.Writer.() -> Unit = {},
  ): Boolean {
    val fd = blob?.let { Blob.create(it) }
    try {
      return write(type, fd, body) { r ->
        try {
          onReply?.invoke(r)
        } finally {
          r.closeFds()
        }
      }
    } finally {
      Blob.close(fd)
    }
  }

  private fun write(
    type: Int,
    fd: FileDescriptor?,
    body: Wire.Writer.() -> Unit,
    onReply: (Reply) -> Unit,
  ): Boolean {
    return writeFiles(type, if (fd == null) emptyArray() else arrayOf(fd), body, onReply)
  }

  /** Sends up to four fds; ownership stays with the caller. */
  fun writeFiles(
    type: Int,
    fds: Array<FileDescriptor>,
    body: Wire.Writer.() -> Unit,
    onReply: (Reply) -> Unit,
  ): Boolean {
    require(fds.size <= 4)
    val s = socket ?: return false
    if (!connected) return false
    val tag = tags.getAndIncrement() and 0x7fffffff
    val msg = Wire.message(type, tag).apply(body).bytes()
    if (msg.size > Wire.MAX_MSG) return false
    pending[tag] = onReply
    return try {
      write(s, msg, fds)
      true
    } catch (e: IOException) {
      pending.remove(tag)
      Log.w(TAG, "service $service: send failed: ${e.message}")
      false
    }
  }

  private fun write(s: LocalSocket, msg: ByteArray, fds: Array<FileDescriptor>) {
    synchronized(writeLock) {
      // LocalSocket keeps outbound fds until replaced, so set them for every write.
      s.setFileDescriptorsForSend(fds.takeIf { it.isNotEmpty() })
      try {
        s.outputStream.write(msg)
      } finally {
        s.setFileDescriptorsForSend(null)
      }
    }
  }

  private fun run() {
    if (latencySensitive) raisePriority()
    var backoff = MIN_BACKOFF_MS
    var lastError: String? = null
    while (true) {
      val s = LocalSocket(LocalSocket.SOCKET_SEQPACKET)
      try {
        s.connect(LocalSocketAddress(path, LocalSocketAddress.Namespace.FILESYSTEM))
        socket = s
        session(s)
        backoff = MIN_BACKOFF_MS
        lastError = null
      } catch (e: IOException) {
        val msg = e.message ?: e.toString()
        if (msg != lastError) Log.i(TAG, "bridge service $service: $msg (retrying)")
        lastError = msg
      } catch (e: Exception) {
        Log.e(TAG, "bridge service $service failed", e)
      } finally {
        val was = connected
        connected = false
        socket = null
        pending.clear()
        try {
          s.close()
        } catch (_: IOException) {}
        if (was) {
          Log.i(TAG, "disconnected from bridge service $service")
          listener.onDisconnected()
        }
      }
      try {
        Thread.sleep(backoff)
      } catch (_: InterruptedException) {}
      backoff = minOf(backoff * 2, MAX_BACKOFF_MS)
    }
  }

  private fun session(s: LocalSocket) {
    val buf = ByteArray(Wire.MAX_MSG + 1)
    write(s, Wire.hello(HELLO_TAG, service, name), emptyArray())
    val input = s.inputStream
    while (true) {
      val n = input.read(buf)
      if (n <= 0) return
      var fds: Array<FileDescriptor> = s.ancillaryFileDescriptors ?: emptyArray()
      try {
        val (type, tag, body) = Wire.parse(buf, n) ?: throw IOException("bad packet ($n bytes)")
        if (type == Wire.REPLY) {
          val status = body.int
          if (tag == HELLO_TAG) {
            if (status != 0) throw IOException("bridge refused service $service: $status")
            connected = true
            Log.i(TAG, "connected to bridge service $service")
            // On its own thread: it may make calls, whose replies this thread reads.
            Thread(listener::onConnected, "innkeeper-bridge-$service-up").start()
            continue
          }
          val handler = pending.remove(tag)
          if (handler != null) {
            val reply = Reply(status, body.slice().order(ByteOrder.LITTLE_ENDIAN), fds)
            fds = emptyArray() // the handler's now
            handler(reply)
          } else if (status != 0) {
            Log.w(TAG, "service $service request $tag failed: $status")
          }
        } else if (type in Wire.EVENT_MIN..Wire.EVENT_MAX && connected) {
          try {
            listener.onEvent(type, body, fds)
          } catch (e: Exception) {
            Log.w(TAG, "service $service event 0x${type.toString(16)}", e)
          }
        }
      } finally {
        fds.forEach(Blob::close)
      }
    }
  }

  // Negative nice values need RLIMIT_NICE, which the rootless session may not grant: take what
  // there is.
  private fun raisePriority() {
    for (prio in
      intArrayOf(Process.THREAD_PRIORITY_URGENT_DISPLAY, Process.THREAD_PRIORITY_DISPLAY)) {
      try {
        Process.setThreadPriority(prio)
        return
      } catch (_: SecurityException) {}
    }
  }

  companion object {
    const val TAG = "InnkeeperBridge"
    const val SOCKET = "/dev/socket/innkeeper/bridge.sock"
    const val SERVICE_SYSTEM = 3
    const val SERVICE_DOCUMENTS = 4
    const val SERVICE_APPS = 5
    const val SERVICE_INPUT = 6
    const val SERVICE_HOSTSTATE = 8
    private const val HELLO_TAG = 1
    private const val MIN_BACKOFF_MS = 250L
    private const val MAX_BACKOFF_MS = 5000L
  }
}
