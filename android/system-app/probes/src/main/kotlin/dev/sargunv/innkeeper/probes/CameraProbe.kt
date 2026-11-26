// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.probes

import android.app.Activity
import android.graphics.ImageFormat
import android.hardware.camera2.CameraCaptureSession
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraDevice
import android.hardware.camera2.CameraManager
import android.media.ImageReader
import android.os.Bundle
import android.os.Handler
import android.os.HandlerThread
import android.util.Log
import android.widget.TextView

/**
 * Opens the first camera with Camera2, streams YUV frames into an ImageReader and logs each one's
 * size and the mean luma of its center. The e2e camera check compares that with the private
 * desktop's virtual camera. The tests install with -g, which grants CAMERA.
 */
class CameraProbe : Activity() {
  private val thread = HandlerThread("camera-probe").apply { start() }
  private val handler = Handler(thread.looper)
  private var device: CameraDevice? = null
  private var reader: ImageReader? = null
  private var frames = 0

  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    setContentView(TextView(this).apply { text = "Camera probe" })
    val manager = getSystemService(CameraManager::class.java)
    val ids = manager.cameraIdList
    Log.i(TAG, "cameras ${ids.joinToString(",")}")
    val id = ids.firstOrNull() ?: return
    val facing = manager.getCameraCharacteristics(id).get(CameraCharacteristics.LENS_FACING)
    Log.i(TAG, "camera $id facing $facing")
    val r = ImageReader.newInstance(640, 480, ImageFormat.YUV_420_888, 3)
    reader = r
    r.setOnImageAvailableListener(
      {
        val image = it.acquireLatestImage() ?: return@setOnImageAvailableListener
        image.use { img ->
          val y = img.planes[0]
          var sum = 0L
          var n = 0
          for (row in img.height * 3 / 8 until img.height * 5 / 8) {
            for (col in img.width * 3 / 8 until img.width * 5 / 8) {
              sum += y.buffer.get(row * y.rowStride + col).toInt() and 0xff
              n++
            }
          }
          frames++
          if (frames <= 30 || frames % 30 == 0) {
            Log.i(TAG, "frame $frames ${img.width}x${img.height} luma=${sum / n}")
          }
        }
      },
      handler,
    )
    manager.openCamera(
      id,
      object : CameraDevice.StateCallback() {
        override fun onOpened(d: CameraDevice) {
          device = d
          Log.i(TAG, "opened $id")
          d.createCaptureSession(
            listOf(r.surface),
            object : CameraCaptureSession.StateCallback() {
              override fun onConfigured(s: CameraCaptureSession) {
                val req =
                  d.createCaptureRequest(CameraDevice.TEMPLATE_PREVIEW).apply {
                    addTarget(r.surface)
                  }
                s.setRepeatingRequest(req.build(), null, handler)
              }

              override fun onConfigureFailed(s: CameraCaptureSession) {
                Log.e(TAG, "configure failed")
              }
            },
            handler,
          )
        }

        override fun onDisconnected(d: CameraDevice) {
          Log.i(TAG, "disconnected")
          d.close()
        }

        override fun onError(d: CameraDevice, error: Int) {
          Log.e(TAG, "error $error")
          d.close()
        }
      },
      handler,
    )
  }

  override fun onDestroy() {
    device?.close()
    reader?.close()
    thread.quitSafely()
    super.onDestroy()
  }

  private companion object {
    const val TAG = "InnkeeperCameraProbe"
  }
}
