// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.wm

/** One ImageReader's acquired images. All calls run on its frame handler. */
class RetainedFrames<T>(private val limit: Int, private val close: (T) -> Unit) {
  private val held = LinkedHashMap<Long, T>()
  // RELEASE can beat ImageReader.onImageAvailable. These are credits for one
  // acquisition, never permission to overwrite a later frame of the same buffer.
  private val released = LinkedHashSet<Long>()
  val hasCapacity: Boolean
    get() = held.size < limit

  fun acquire(id: Long, image: T) {
    if (id == 0L) {
      close(image)
      return
    }
    check(id !in held && hasCapacity)
    if (released.remove(id)) close(image) else held[id] = image
  }

  fun release(id: Long) {
    val image = held.remove(id)
    if (image != null) close(image)
    else {
      // A reader queue is small, including buffers retired by a resize.
      if (released.size == 64) released.remove(released.first())
      released.add(id)
    }
  }

  // The metadata page remains authoritative when a bridge restart loses events.
  fun retire(released: (T) -> Boolean) {
    val iterator = held.values.iterator()
    while (iterator.hasNext()) {
      val image = iterator.next()
      if (released(image)) {
        iterator.remove()
        close(image)
      }
    }
  }

  fun clear() {
    held.values.forEach(close)
    held.clear()
    released.clear()
  }
}
