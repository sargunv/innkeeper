// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.integration.clipboard

import android.content.ClipData
import android.content.ClipDescription
import android.content.ClipboardManager
import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.os.Handler
import android.os.Looper
import android.os.PersistableBundle
import android.util.Log
import dev.linuxapk.system.integration.IntegrationFeature
import dev.linuxapk.system.integration.Wire
import java.nio.ByteBuffer
import java.nio.ByteOrder

/**
 * The clipboard, both ways (docs/bridge.md, "Clipboard"): Android's primary clip goes to the host
 * selection, and host selections become Android's primary clip. Text both ways, and images (an
 * Android clip with an `image/` URI; a host PNG becomes a clip URI served by [ClipImageProvider]).
 * Each side remembers what it last got from the other, so a change never bounces back.
 *
 * `ClipDescription.EXTRA_IS_SENSITIVE` maps to KDE's password-manager hint and back.
 */
class ClipboardSync(private val context: Context, private val feature: IntegrationFeature) {
  private val cm by lazy { context.getSystemService(ClipboardManager::class.java) }
  private val main = Handler(Looper.getMainLooper())

  /** What the host last gave us (text, or the image's size and hash). */
  @Volatile private var fromHost: String? = null

  /** What we last sent the host (the listener can fire more than once per change). */
  private var toHost: String? = null

  private val listener = ClipboardManager.OnPrimaryClipChangedListener {
    feature.handler.post { onAndroidClip() }
  }

  fun start() {
    // The listener is called on the thread that registered it, which needs a looper.
    main.post { cm.addPrimaryClipChangedListener(listener) }
  }

  fun stop() {
    main.post { cm.removePrimaryClipChangedListener(listener) }
  }

  private fun onAndroidClip() {
    val clip =
      try {
        cm.primaryClip
      } catch (e: Exception) {
        Log.w(TAG, "reading the clip: $e")
        null
      } ?: return
    if (clip.itemCount == 0) return
    val desc = clip.description
    val sensitive = desc.extras?.getBoolean(ClipDescription.EXTRA_IS_SENSITIVE) == true
    val item = clip.getItemAt(0)
    val uri = item.uri
    val mime =
      if (uri != null)
        (context.contentResolver.getType(uri)
          ?: desc.takeIf { it.mimeTypeCount > 0 }?.getMimeType(0))
      else null
    if (uri != null && mime?.startsWith("image/") == true) {
      val img = loadImage(uri) ?: return
      val key = imageKey(img)
      if (fromHost == key || toHost == key) return
      toHost = key
      val blob = Wire.Writer(img.bytes.size + 8).image(img).bytes()
      send(Wire.CLIP_IMAGE, sensitive, blob)
      return
    }
    val text = item.coerceToText(context)?.toString() ?: return
    if (text.isEmpty() || text == fromHost || text == toHost) return
    toHost = text
    send(Wire.CLIP_TEXT, sensitive, Wire.truncateUtf8(text, 1 shl 20))
  }

  private fun send(kind: Int, sensitive: Boolean, blob: ByteArray) {
    val ok =
      feature.client.request(
        Wire.CLIP_SET,
        { u8(kind).u8(if (sensitive) 1 else 0).u16(0).u32(blob.size) },
        blob,
      ) { status, _ ->
        if (status != 0) Log.w(TAG, "clipboard to the host: $status")
      }
    if (ok) Log.i(TAG, "clip to the host (kind $kind, ${blob.size} bytes)")
  }

  private fun loadImage(uri: android.net.Uri): Wire.Rgba? =
    try {
      val cr = context.contentResolver
      val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
      cr.openInputStream(uri)?.use { BitmapFactory.decodeStream(it, null, bounds) }
      var sample = 1
      while (maxOf(bounds.outWidth, bounds.outHeight) / sample > MAX_SIDE) sample *= 2
      val opts = BitmapFactory.Options().apply { inSampleSize = sample }
      val bmp = cr.openInputStream(uri)?.use { BitmapFactory.decodeStream(it, null, opts) }
      bmp?.let { Wire.rgba(it, MAX_SIDE) }
    } catch (e: Exception) {
      Log.w(TAG, "reading clip image $uri: $e")
      null
    }

  private fun imageKey(img: Wire.Rgba) =
    "image ${img.width}x${img.height} ${img.bytes.contentHashCode()}"

  /** A host selection ([kind] text or image; [data] is the blob's content). */
  fun onHostClip(kind: Int, sensitive: Boolean, data: ByteArray) {
    toHost = null
    val extras = PersistableBundle().apply { putBoolean(ClipDescription.EXTRA_IS_SENSITIVE, true) }
    val clip =
      when (kind) {
        Wire.CLIP_TEXT -> {
          val text = String(data, Charsets.UTF_8)
          if (text == currentText()) {
            fromHost = text
            return
          }
          fromHost = text
          ClipData.newPlainText("Desktop", text)
        }
        Wire.CLIP_IMAGE -> {
          val img = decodeImage(data) ?: return
          fromHost = imageKey(img)
          val uri = ClipImageProvider.store(context, toBitmap(img)) ?: return
          ClipData.newUri(context.contentResolver, "Desktop image", uri)
        }
        else -> return
      }
    if (sensitive) clip.description.extras = extras
    try {
      cm.setPrimaryClip(clip)
      Log.i(TAG, "clip from the host (kind $kind, ${data.size} bytes)")
    } catch (e: Exception) {
      Log.w(TAG, "setting the clip: $e")
    }
  }

  private fun currentText(): String? =
    try {
      cm.primaryClip?.takeIf { it.itemCount > 0 }?.getItemAt(0)?.text?.toString()
    } catch (_: Exception) {
      null
    }

  private fun decodeImage(b: ByteArray): Wire.Rgba? {
    val r = ByteBuffer.wrap(b).order(ByteOrder.LITTLE_ENDIAN)
    if (r.remaining() < 4) return null
    val w = r.short.toInt() and 0xffff
    val h = r.short.toInt() and 0xffff
    if (w == 0 || h == 0 || r.remaining() != w * h * 4) return null
    val px = ByteArray(w * h * 4)
    r.get(px)
    return Wire.Rgba(w, h, px)
  }

  private fun toBitmap(img: Wire.Rgba): Bitmap {
    val px = IntArray(img.width * img.height)
    val b = img.bytes
    for (i in px.indices) {
      px[i] =
        ((b[i * 4 + 3].toInt() and 0xff) shl 24) or
          ((b[i * 4].toInt() and 0xff) shl 16) or
          ((b[i * 4 + 1].toInt() and 0xff) shl 8) or
          (b[i * 4 + 2].toInt() and 0xff)
    }
    return Bitmap.createBitmap(px, img.width, img.height, Bitmap.Config.ARGB_8888)
  }

  companion object {
    const val TAG = "LinuxApkClipboard"
    private const val MAX_SIDE = 2048
  }
}
