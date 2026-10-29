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
import android.view.KeyEvent
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
  private var keysDisplay = -1
  private var buttons = 0
  private val held = HashSet<Int>()
  private val sentKeys = HashSet<Int>()
  private var sentButtons = 0
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

  @Synchronized
  fun focus(display: Int) {
    if (keysDisplay == display) return
    dm.getDisplay(display)?.let {
      association.invoke(
        im,
        "linuxapk/keyboard",
        Display::class.java.getMethod("getUniqueId").invoke(it) as String,
      )
      keysDisplay = display
    }
  }

  // Preserve the route chosen at DOWN through UP. Layout remapping, IME text
  // corrections, synthetic Back and compositor repeats keep the injection path.
  @Synchronized
  fun key(e: OutKey): Boolean {
    if (e.repeatCount > 0) return false
    val down = e.action == KeyEvent.ACTION_DOWN
    val real =
      if (down && e.repeatCount == 0) {
        e.text == null &&
          e.keyCode == EvdevKeymap.keycode(e.scanCode) &&
          e.scanCode !in listOf(58, 69, 70) &&
          e.metaState ==
            KeyEvent.normalizeMetaState(
              (held + e.scanCode).fold(0) { m, code -> m or modifier(code) },
            )
      } else held.contains(e.scanCode)
    if (!real) return false
    if (down) {
      focus(e.displayId)
      val keyboard =
        im.inputDeviceIds.toList().mapNotNull(im::getInputDevice).firstOrNull {
          it.name == "Linux.apk Keyboard"
        }
      // Never buffer a key behind association: the next translated key may need
      // injection and must not overtake this one.
      if (keyboard == null || (displayId.invoke(keyboard) as Int) != e.displayId) return false
      held.add(e.scanCode)
    } else held.remove(e.scanCode)
    if (send(0, e.eventTime, listOf(Triple(1, e.scanCode, if (down) 1 else 0)))) {
      if (down) sentKeys.add(e.scanCode) else sentKeys.remove(e.scanCode)
    }
    return true
  }

  fun motion(e: OutMotion): Boolean {
    if (
      e.source != InputDevice.SOURCE_MOUSE ||
        e.classification != 0 ||
        e.pointers.singleOrNull()?.toolType != MotionEvent.TOOL_TYPE_MOUSE
    )
      return false
    handler.post {
      val meta = synchronized(this) { nativeMeta() }
      if (!pointer.native(e.action and MotionEvent.ACTION_MASK, e.buttonState, e.metaState, meta)) {
        inject(e)
        return@post
      }
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
        connection.send(0x0608) { u32(e.displayId).i32(Math.round(step.x)).i32(Math.round(step.y)) }
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
        if (send(1, e.eventTime, events)) sentButtons = e.buttonState
      }
    }
  }

  // A bridge reconnect must release keys whose UP could not be delivered.
  @Synchronized
  fun reset() {
    held.clear()
    val now = SystemClock.uptimeMillis()
    for (batch in sentKeys.toList().chunked(32)) {
      if (send(0, now, batch.map { Triple(1, it, 0) })) sentKeys.removeAll(batch.toSet())
    }
    handler.post {
      queue.clear()
      val releases =
        listOf(1 to 272, 2 to 273, 4 to 274)
          .filter { (mask, _) -> sentButtons and mask != 0 }
          .map { (_, code) -> Triple(1, code, 0) }
      if (releases.isNotEmpty() && send(1, now, releases)) sentButtons = 0
      buttons = 0
      pointer.reset()
    }
  }

  private fun nativeMeta(): Int =
    KeyEvent.normalizeMetaState(held.fold(0) { m, code -> m or modifier(code) })

  private fun modifier(scanCode: Int): Int =
    when (scanCode) {
      42 -> KeyEvent.META_SHIFT_LEFT_ON
      54 -> KeyEvent.META_SHIFT_RIGHT_ON
      29 -> KeyEvent.META_CTRL_LEFT_ON
      97 -> KeyEvent.META_CTRL_RIGHT_ON
      56 -> KeyEvent.META_ALT_LEFT_ON
      100 -> KeyEvent.META_ALT_RIGHT_ON
      125 -> KeyEvent.META_META_LEFT_ON
      126 -> KeyEvent.META_META_RIGHT_ON
      else -> 0
    }

  private fun send(device: Int, time: Long, events: List<Triple<Int, Int, Int>>): Boolean =
    connection.send(0x0607) {
      u32(device).u64(time * 1000).u32(events.size)
      for ((type, code, value) in events) u16(type).u16(code).i32(value)
    }
}
