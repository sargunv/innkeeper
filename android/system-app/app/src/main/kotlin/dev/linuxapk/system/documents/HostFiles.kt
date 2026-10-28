// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.documents

import android.os.ParcelFileDescriptor
import android.util.Log
import dev.linuxapk.system.bridge.BridgeConnection
import dev.linuxapk.system.integration.Blob
import java.io.FileDescriptor
import java.io.FileNotFoundException
import java.nio.ByteBuffer
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicInteger

/**
 * The bridge's documents service (docs/bridge.md, "Documents service"): host files and folders the
 * user picks in the desktop's file chooser. Android only ever sees grant ids and file descriptors,
 * never host paths. One process-wide connection, shared by [PickerActivity] and
 * [HostDocumentsProvider].
 */
object HostFiles {
  const val TAG = "LinuxApkDocs"

  // Requests and events (host/bridge/src/documents/proto.rs).
  private const val PICK = 0x0401
  private const val OPEN = 0x0402
  private const val STAT = 0x0403
  private const val LIST = 0x0404
  private const val CREATE = 0x0405
  private const val DELETE = 0x0406
  private const val RENAME = 0x0407
  private const val EV_PICKED = 0x4401
  private const val EV_PICK_DONE = 0x4402

  const val MODE_OPEN = 1
  const val MODE_OPEN_MULTIPLE = 2
  const val MODE_SAVE = 3
  const val MODE_FOLDER = 4

  const val OPEN_READ = 1
  const val OPEN_WRITE = 2
  const val OPEN_READ_WRITE = 3
  const val OPEN_APPEND = 4

  private const val MAX_LIST_BYTES = 8 shl 20
  private const val EIO = -5

  data class Picked(val grant: Long, val dir: Boolean, val writable: Boolean, val name: String)

  data class Meta(
    val dir: Boolean,
    val size: Long,
    val mtimeMs: Long,
    val writable: Boolean,
    val name: String,
  )

  /** What a pick waits for: its items, then the end with a status (0, -ECANCELED, -EIO). */
  interface PickListener {
    fun onDone(status: Int, items: List<Picked>)
  }

  private class Pending(val listener: PickListener) {
    val items = ArrayList<Picked>()
  }

  private val requests = AtomicInteger(1)
  private val pending = ConcurrentHashMap<Int, Pending>()
  private val channel by lazy {
    BridgeConnection(
        BridgeConnection.SERVICE_DOCUMENTS,
        "Linux.apk System documents",
        object : BridgeConnection.Listener {
          override fun onDisconnected() = failAll()

          override fun onEvent(type: Int, body: ByteBuffer, fds: Array<FileDescriptor>) =
            this@HostFiles.onEvent(type, body, fds)
        },
      )
      .also { it.start() }
  }

  fun start() {
    channel
  }

  val connected
    get() = channel.connected

  /** Opens the desktop's file chooser over the window of [display]; returns false if it can't. */
  fun pick(
    mode: Int,
    write: Boolean,
    display: Int,
    title: String,
    name: String,
    mimes: List<String>,
    listener: PickListener,
  ): Boolean {
    val id = requests.getAndIncrement()
    pending[id] = Pending(listener)
    val r =
      channel.call(PICK) {
        u32(id)
        u8(mode)
        u8(if (write) 1 else 0)
        u32(maxOf(display, 0))
        name(title)
        name(name)
        val ms = mimes.filter(::validMime).take(16)
        u8(ms.size)
        ms.forEach { name(it) }
      }
    r?.closeFds()
    if (r?.ok != true) {
      pending.remove(id)
      Log.w(TAG, "pick refused: ${r?.status}")
      return false
    }
    return true
  }

  private fun validMime(m: String) = Regex("""[\w!#$&^.+-]+/(\*|[\w!#$&^.+-]+)""").matches(m)

  private fun onEvent(type: Int, b: ByteBuffer, fds: Array<FileDescriptor>) {
    when (type) {
      EV_PICKED -> {
        val req = b.int
        val grant = b.long
        val dir = b.get().toInt() == 1
        val writable = b.get().toInt() == 1
        val name = readText(b)
        pending[req]?.items?.add(Picked(grant, dir, writable, name))
      }
      EV_PICK_DONE -> {
        val req = b.int
        val status = b.int
        val p = pending.remove(req) ?: return
        p.listener.onDone(status, p.items)
      }
    }
  }

  // The bridge went away: its picks are gone, so end them.
  private fun failAll() {
    for (id in pending.keys.toList()) pending.remove(id)?.listener?.onDone(EIO, emptyList())
  }

  /** Ends a pick the app abandoned (its activity finished). */
  fun abandon(listener: PickListener) {
    pending.entries.removeIf { it.value.listener === listener }
  }

  private fun readText(b: ByteBuffer): String {
    val n = b.short.toInt() and 0xffff
    require(n <= b.remaining()) { "short text" }
    val bytes = ByteArray(n)
    b.get(bytes)
    return String(bytes, Charsets.UTF_8)
  }

  private fun readMeta(b: ByteBuffer) =
    Meta(
      dir = b.get().toInt() == 1,
      size = b.long,
      mtimeMs = b.long,
      writable = b.get().toInt() == 1,
      name = readText(b),
    )

  private fun fail(what: String, status: Int?): Nothing =
    throw FileNotFoundException("$what: ${status ?: "bridge unavailable"}")

  fun open(grant: Long, path: String, mode: Int): ParcelFileDescriptor {
    val r =
      channel.call(OPEN) {
        u64(grant)
        u8(mode)
        text16(path)
      }
    if (r?.ok != true || r.fds.size != 1) {
      r?.closeFds()
      fail("open", r?.status)
    }
    return try {
      ParcelFileDescriptor.dup(r.fds[0])
    } finally {
      r.closeFds()
    }
  }

  fun stat(grant: Long, path: String): Meta {
    val r =
      channel.call(STAT) {
        u64(grant)
        text16(path)
      }
    r?.closeFds()
    if (r?.ok != true) fail("stat", r?.status)
    return readMeta(r.body)
  }

  fun list(grant: Long, path: String): List<Meta> {
    val r =
      channel.call(LIST) {
        u64(grant)
        text16(path)
      }
    if (r?.ok != true || r.fds.size != 1) {
      r?.closeFds()
      fail("list", r?.status)
    }
    try {
      val len = r.body.int
      val count = r.body.int
      val b =
        ByteBuffer.wrap(Blob.read(r.fds[0], len, MAX_LIST_BYTES))
          .order(java.nio.ByteOrder.LITTLE_ENDIAN)
      return List(count) { readMeta(b) }
    } finally {
      r.closeFds()
    }
  }

  /** Returns the name the bridge used (it may add " (1)"). */
  fun create(grant: Long, path: String, dir: Boolean, name: String): String {
    val r =
      channel.call(CREATE) {
        u64(grant)
        text16(path)
        u8(if (dir) 1 else 0)
        text16(name)
      }
    r?.closeFds()
    if (r?.ok != true) fail("create", r?.status)
    return readText(r.body)
  }

  fun delete(grant: Long, path: String) {
    val r =
      channel.call(DELETE) {
        u64(grant)
        text16(path)
      }
    r?.closeFds()
    if (r?.ok != true) fail("delete", r?.status)
  }

  fun rename(grant: Long, path: String, name: String) {
    val r =
      channel.call(RENAME) {
        u64(grant)
        text16(path)
        text16(name)
      }
    r?.closeFds()
    if (r?.ok != true) fail("rename", r?.status)
  }

  /** u16 length + UTF-8 (paths and file names; the bridge checks them). */
  private fun dev.linuxapk.system.integration.Wire.Writer.text16(s: String) {
    val b = s.toByteArray(Charsets.UTF_8)
    require(b.size <= 400) { "path too long" }
    u16(b.size)
    b.forEach { u8(it.toInt()) }
  }
}
