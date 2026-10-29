// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.input

import android.content.Context
import android.graphics.Point
import android.graphics.PointF
import android.hardware.display.DisplayManager
import android.hardware.input.InputManager
import android.os.Handler
import android.os.HandlerThread
import android.os.SystemClock
import android.view.Display
import android.view.InputDevice
import android.view.MotionEvent
import dev.linuxapk.system.bridge.BridgeConnection

/** The real evdev path, after the host-layout/shortcut translator made its decisions. */
class VirtualInput(
  private val context: Context,
  private val connection: BridgeConnection,
  private val inject: (OutEvent) -> Unit,
) {
  private val im = context.getSystemService(InputManager::class.java)
  private val dm = context.getSystemService(DisplayManager::class.java)
  private val thread = HandlerThread("LinuxApkEvdev").apply { start() }
  private val handler = Handler(thread.looper)
  private val association =
    im.javaClass.getMethod("addUniqueIdAssociationByPort", String::class.java, String::class.java)
  private val scaling =
    im.javaClass.getMethod(
      "setMouseScalingEnabled",
      Boolean::class.javaPrimitiveType,
      Int::class.javaPrimitiveType,
    )
  private val position = im.javaClass.getMethod("getCursorPosition", Int::class.javaPrimitiveType)
  private val displayId = InputDevice::class.java.getMethod("getAssociatedDisplayId")
  private var mouseDisplay = -1
  private var buttons = 0
  private var sentButtons = 0
  private var nativeHover = false
  private val queue = ArrayDeque<OutMotion>()
  private val pointer = EvdevPointer()

  init {
    im.registerInputDeviceListener(
      object : InputManager.InputDeviceListener {
        override fun onInputDeviceAdded(id: Int) = drain()

        override fun onInputDeviceChanged(id: Int) = drain()

        override fun onInputDeviceRemoved(id: Int) = Unit
      },
      handler,
    )
  }

  fun motion(e: OutMotion): Boolean {
    if (
      e.source != InputDevice.SOURCE_MOUSE ||
        e.classification != 0 ||
        e.pointers.singleOrNull()?.toolType != MotionEvent.TOOL_TYPE_MOUSE
    )
      return false
    handler.post {
      // Keys are injected with their host modifiers. InputReader's evdev
      // keyboard state stays unmodified, so modifier-sensitive mouse actions
      // use injection too. Android assigns all injected events device ID -1.
      if (!pointer.native(e.action and MotionEvent.ACTION_MASK, e.buttonState, e.metaState, 0)) {
        endNativeHover(e.eventTime)
        inject(e)
        pointer.injected(e)
        return@post
      }
      pointer.exitInjected(e.eventTime)?.let(inject)
      if (mouseDisplay != e.displayId) {
        mouseDisplay = e.displayId
        queue.clear()
        pointer.reset()
        dm.getDisplay(e.displayId)?.let {
          scaling.invoke(im, false, e.displayId)
          association.invoke(
            im,
            "linuxapk/mouse",
            Display::class.java.getMethod("getUniqueId").invoke(it) as String,
          )
        }
      }
      queue.add(e)
      drain()
    }
    return true
  }

  private fun drain() {
    val mouse =
      im.inputDeviceIds.toList().mapNotNull(im::getInputDevice).firstOrNull {
        it.name == "Linux.apk Mouse"
      } ?: return
    if ((displayId.invoke(mouse) as Int) != mouseDisplay) return
    while (queue.isNotEmpty()) {
      val e = queue.removeFirst()
      val action = e.action and MotionEvent.ACTION_MASK
      if (action == MotionEvent.ACTION_HOVER_EXIT) {
        endNativeHover(e.eventTime)
        continue
      }
      val p = e.pointers[0]
      val events = ArrayList<Triple<Int, Int, Int>>()
      if (
        action == MotionEvent.ACTION_HOVER_ENTER ||
          action == MotionEvent.ACTION_HOVER_MOVE ||
          action == MotionEvent.ACTION_MOVE
      ) {
        val current = position.invoke(im, e.displayId) as? PointF
        if (current == null) {
          queue.addFirst(e)
          return
        }
        val size = Point()
        dm.getDisplay(e.displayId)?.getRealSize(size)
        val step = pointer.move(p.x, p.y, current.x, current.y, size.x, size.y)
        events += Triple(2, 0, step.dx)
        events += Triple(2, 1, step.dy)
        connection.send(0x0609) { u32(e.displayId).i32(Math.round(step.x)).i32(Math.round(step.y)) }
      }
      for ((mask, code) in listOf(1 to 272, 2 to 273, 4 to 274)) {
        if ((buttons xor e.buttonState) and mask != 0)
          events += Triple(1, code, if (e.buttonState and mask != 0) 1 else 0)
      }
      buttons = e.buttonState
      if (action == MotionEvent.ACTION_SCROLL) {
        if (p.vscroll != 0f) events += Triple(2, 11, Math.round(p.vscroll * 120))
        if (p.hscroll != 0f) events += Triple(2, 12, Math.round(p.hscroll * 120))
      }
      if (events.isNotEmpty()) {
        if (send(1, e.eventTime, events)) {
          sentButtons = e.buttonState
          nativeHover = true
        }
      }
    }
  }

  private fun endNativeHover(time: Long) {
    // A relative mouse has no out-of-range coordinate. End this discontinuous
    // stream with evdev's resync marker; InputReader resets the device, and
    // InputDispatcher emits HOVER_EXIT under its real ID. An injected exit
    // cannot do this: Android rewrites injected device IDs to -1.
    if (nativeHover && send(1, time, listOf(Triple(0, 3, 0)))) nativeHover = false
    pointer.reset()
  }

  // Release buttons whose UP could not be delivered across a disconnect.
  fun reset() {
    val now = SystemClock.uptimeMillis()
    handler.post {
      queue.clear()
      val releases =
        listOf(1 to 272, 2 to 273, 4 to 274)
          .filter { (mask, _) -> sentButtons and mask != 0 }
          .map { (_, code) -> Triple(1, code, 0) }
      if (releases.isNotEmpty() && send(1, now, releases)) sentButtons = 0
      buttons = 0
      endNativeHover(now)
      pointer.exitInjected(now)?.let(inject)
    }
  }

  private fun send(device: Int, time: Long, events: List<Triple<Int, Int, Int>>): Boolean =
    connection.send(0x0608) {
      u32(device).u64(time * 1000).u32(events.size)
      for ((type, code, value) in events) u16(type).u16(code).i32(value)
    }
}
