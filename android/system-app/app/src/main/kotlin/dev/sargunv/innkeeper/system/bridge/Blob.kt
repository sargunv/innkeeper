// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.bridge

import android.system.Os
import android.system.OsConstants
import java.io.FileDescriptor

/** Sealed memfds, which carry payloads too big for a message across the socket. */
object Blob {
  private const val MFD_ALLOW_SEALING = 2
  private const val F_ADD_SEALS = 1033
  private const val F_SEAL_SEAL = 1
  private const val F_SEAL_SHRINK = 2
  private const val F_SEAL_GROW = 4
  private const val F_SEAL_WRITE = 8

  /** A memfd holding [bytes], sealed against writes and resizing. The caller closes it. */
  fun create(bytes: ByteArray): FileDescriptor {
    val fd = Os.memfd_create("innkeeper-blob", OsConstants.MFD_CLOEXEC or MFD_ALLOW_SEALING)
    try {
      var off = 0
      while (off < bytes.size) off += Os.write(fd, bytes, off, bytes.size - off)
      Os.fcntlInt(fd, F_ADD_SEALS, F_SEAL_SHRINK or F_SEAL_GROW or F_SEAL_WRITE or F_SEAL_SEAL)
    } catch (e: Exception) {
      Os.close(fd)
      throw e
    }
    return fd
  }

  /** Reads [len] bytes (at most [max]) from a memfd the bridge sent. */
  fun read(fd: FileDescriptor, len: Int, max: Int): ByteArray {
    require(len in 0..max) { "blob too long" }
    require(Os.fstat(fd).st_size >= len) { "blob shorter than declared" }
    val b = ByteArray(len)
    var off = 0
    while (off < len) {
      val n = Os.pread(fd, b, off, len - off, off.toLong())
      require(n > 0) { "short blob" }
      off += n
    }
    return b
  }

  fun close(fd: FileDescriptor?) {
    try {
      if (fd != null && fd.valid()) Os.close(fd)
    } catch (_: Exception) {}
  }
}
