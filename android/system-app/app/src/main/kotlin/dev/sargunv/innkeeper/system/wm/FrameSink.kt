// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.wm

import android.hardware.HardwareBuffer
import android.media.Image
import android.media.ImageReader
import android.os.Handler
import dev.sargunv.innkeeper.system.input.HostEvent
import dev.sargunv.innkeeper.system.input.HostInputClient

/** Keeps the producer's pixels owned while the host compositor reads their dma-buf. */
class FrameSink(
  width: Int,
  height: Int,
  private val handler: Handler,
  private val input: HostInputClient,
) : HostInputClient.Listener {
  companion object {
    init {
      System.loadLibrary("frame_sink")
    }
  }

  private external fun presentationToken(buffer: HardwareBuffer): Long

  private val reader =
    ImageReader.Builder(width, height)
      .setDefaultHardwareBufferFormat(HardwareBuffer.RGBA_8888)
      .setMaxImages(3)
      .setUsage(
        HardwareBuffer.USAGE_GPU_SAMPLED_IMAGE or
          HardwareBuffer.USAGE_GPU_COLOR_OUTPUT or
          HardwareBuffer.USAGE_COMPOSER_OVERLAY or
          // The desktop bridge reads these pixels for hidden-window snapshots
          // and wl_shm fallback. Keep this consumer's buffers CPU-readable.
          HardwareBuffer.USAGE_CPU_READ_RARELY,
      )
      .build()
  private val frames = RetainedFrames<Image>(reader.maxImages) { it.close() }
  @Volatile private var display = -1
  private var closed = false
  val surface
    get() = reader.surface

  init {
    reader.setOnImageAvailableListener({ drain() }, handler)
    input.addListener(this)
  }

  fun bind(display: Int) {
    this.display = display
  }

  private fun drain() {
    if (closed) return
    // HWC clears leases when its connection resets, even if the new bridge
    // has no display mapping yet and cannot forward the release event.
    frames.retire { image -> image.hardwareBuffer!!.use { presentationToken(it) == 0L } }
    // HWC hands at most two images to Wayland; every excess frame is
    // released before it reaches Wayland. Wait for that release notification
    // rather than closing an image the compositor could still be reading.
    while (frames.hasCapacity) {
      val image = reader.acquireNextImage() ?: break
      val id = image.hardwareBuffer!!.use { presentationToken(it) }
      frames.acquire(id, image)
    }
  }

  override fun onConnected() {
    // An input-only reconnect must preserve live Wayland leases. If HWC has
    // not reset yet, the next image callback will reclaim its old generation.
    handler.post { drain() }
  }

  override fun onHostEvent(e: HostEvent) {
    if (e is HostEvent.WindowUnmapped && e.display == display)
      handler.post {
        if (!closed) {
          frames.clear()
          drain()
        }
      }
  }

  override fun onImageReleased(display: Int, buffer: Long) {
    if (display != this.display) return
    handler.post {
      if (!closed) {
        frames.release(buffer)
        drain()
      }
    }
  }

  fun close() {
    input.removeListener(this)
    handler.post {
      if (closed) return@post
      closed = true
      frames.clear()
      reader.close()
    }
  }
}
