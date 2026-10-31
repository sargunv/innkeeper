// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.input

import android.content.Context
import android.graphics.Point
import android.graphics.PointF
import android.hardware.display.DisplayManager
import android.hardware.input.IInputManager
import android.hardware.input.InputManager
import android.os.Handler
import android.os.HandlerThread
import android.os.ServiceManager
import android.os.SystemClock
import android.view.Display
import android.view.InputDevice
import android.view.MotionEvent
import dev.sargunv.innkeeper.system.bridge.BridgeConnection

/**
 * The real evdev path, after the host-layout/shortcut translator made its decisions.
 *
 * Modifiers and every ordinary mouse event go through the broker's evdev devices, because Android
 * 17's `KeyboardInputMapper::updateMetaStateIfNeeded` updates InputReader's global meta state only
 * for real devices and `CursorInputMapper::sync` reads it, so a modifier held during a drag reaches
 * the mouse; injected keys never update it. Ordinary keys stay on the injection stream with
 * explicit meta state, so typing doesn't depend on how the two streams interleave. Each window's
 * mouse is associated with its display by unique id (Android 17's display-association,
 * mouse-scaling and cursor-position test APIs) and never reassigned, so in-flight records keep
 * their destination; records wait in host order until the association exists. The broker sends a
 * batch only after EventHub read the last one, so epoll can't reorder modifiers and motion.
 * Relative motion can't leave a window, so leaving ends the stream with `SYN_DROPPED` +
 * `SYN_REPORT`, which makes InputReader reset the device and emit HOVER_EXIT.
 */
class VirtualInput(
  private val context: Context,
  private val connection: BridgeConnection,
) {
  private val im = context.getSystemService(InputManager::class.java)
  private val dm = context.getSystemService(DisplayManager::class.java)
  private val thread = HandlerThread("InnkeeperEvdev").apply { start() }
  private val handler = Handler(thread.looper)
  private val inputService = IInputManager.Stub.asInterface(ServiceManager.getService("input"))
  private val position = im.javaClass.getMethod("getCursorPosition", Int::class.javaPrimitiveType)
  private val displayId = InputDevice::class.java.getMethod("getAssociatedDisplayId")
  private var capture: Boolean? = null
  private var queuedMeta = 0
  private val keyboard = EvdevModifiers()

  private class Mouse(val display: Int) {
    val device = display + 1
    val port = "innkeeper/mouse/$display"
    val pointer = EvdevPointer()
    val relative = EvdevRelative()
    var buttons = 0
    var sentButtons = 0
    var nativeHover = false
  }

  private val mice = HashMap<Int, Mouse>()
  private val removals = EvdevRemovals()

  private sealed interface Pending {
    data class Modifiers(val meta: Int, val time: Long) : Pending

    data class Mouse(val event: OutMotion) : Pending

    data class Relative(val event: HostEvent.PointerRelative) : Pending
  }

  private val queue = ArrayDeque<Pending>()

  init {
    dm.registerDisplayListener(
      object : DisplayManager.DisplayListener {
        override fun onDisplayAdded(id: Int) = drain()

        override fun onDisplayChanged(id: Int) = drain()

        override fun onDisplayRemoved(id: Int) {
          removeMouse(id)
        }
      },
      handler,
    )
    im.registerInputDeviceListener(
      object : InputManager.InputDeviceListener {
        override fun onInputDeviceAdded(id: Int) {
          captureChanged()
          drain()
        }

        override fun onInputDeviceChanged(id: Int) {
          captureChanged()
          drain()
        }

        override fun onInputDeviceRemoved(id: Int) = captureChanged()
      },
      handler,
    )
  }

  // Android 17 CursorInputMapper::configureOnPointerCapture changes mSource and
  // bumps the device generation. InputDeviceListener and sources are public SDK
  // APIs; unlike a hidden cursor, SOURCE_MOUSE_RELATIVE unambiguously means capture.
  // Capture is global in InputReader, so the bridge selects its focused window.
  private fun captureChanged() {
    val enabled =
      im.inputDeviceIds.toList().mapNotNull(im::getInputDevice).any {
        it.name.startsWith("Innkeeper Mouse ") &&
          it.supportsSource(InputDevice.SOURCE_MOUSE_RELATIVE)
      }
    if (capture == enabled) return
    capture = enabled
    android.util.Log.i(HostInputClient.TAG, "pointer capture=$enabled")
    for (mouse in mice.values) {
      mouse.pointer.reset()
      mouse.relative.reset()
    }
    connection.send(0x060b) { u32(if (enabled) 1 else 0) }
  }

  fun relative(e: HostEvent.PointerRelative) {
    handler.post {
      queue.add(Pending.Relative(e))
      drain()
    }
  }

  fun modifiers(meta: Int, time: Long) {
    handler.post {
      if (meta != queuedMeta) {
        queuedMeta = meta
        queue.add(Pending.Modifiers(meta, time))
      }
      drain()
    }
  }

  fun motion(e: OutMotion): Boolean {
    if (!EvdevPointer.accepts(e)) return false
    handler.post {
      queue.add(Pending.Mouse(e))
      drain()
    }
    return true
  }

  /** A window owns one mouse for its entire lifetime; its port is never reassociated. */
  fun addDisplay(id: Int) {
    handler.post {
      val display = dm.getDisplay(id) ?: return@post
      val mouse =
        mice.getOrPut(id) {
          Mouse(id).also {
            inputService.setMouseScalingEnabled(false, id)
            inputService.addUniqueIdAssociationByPort(
              it.port,
              Display::class.java.getMethod("getUniqueId").invoke(display) as String,
            )
          }
        }
      connection.send(0x060a) { u32(mouse.device).u32(1) }
      drain()
    }
  }

  fun removeDisplay(id: Int) {
    handler.post { removeMouse(id) }
  }

  private fun removeMouse(id: Int) {
    queue.removeAll {
      (it is Pending.Mouse && it.event.displayId == id) ||
        (it is Pending.Relative && it.event.display == id)
    }
    mice.remove(id)?.let { mouse ->
      removals.add(mouse.device)
      inputService.removeUniqueIdAssociationByPort(mouse.port)
    }
    flushRemovals()
    drain()
  }

  // The broker survives a bridge restart. Keep a closed window's device until
  // the bridge has acknowledged its removal, and retry before replaying live mice.
  private fun flushRemovals() {
    removals.flush { device ->
      val reply = connection.call(0x060a, timeoutMs = 500) { u32(device).u32(0) }
      reply?.closeFds()
      reply?.ok == true
    }
  }

  private fun ready(mouse: Mouse): Boolean =
    im.inputDeviceIds.toList().mapNotNull(im::getInputDevice).any {
      it.name == "Innkeeper Mouse ${mouse.display}" &&
        (displayId.invoke(it) as Int) == mouse.display
    }

  private fun drain() {
    while (queue.isNotEmpty()) {
      val pending = queue.first()
      if (pending is Pending.Modifiers) {
        if (!keyboard.sync(pending.meta) { send(0, pending.time, it) }) return
        queue.removeFirst()
        continue
      }
      if (pending is Pending.Relative) {
        val e = pending.event
        val mouse = mice[e.display]
        queue.removeFirst()
        if (mouse != null && capture == true) {
          val events = mouse.relative.move(e.dx, e.dy)
          if (events.isNotEmpty()) send(mouse.device, SystemClock.uptimeMillis(), events)
        }
        continue
      }
      val e = (pending as Pending.Mouse).event
      // A removal callback can precede a late report from the host. Never let
      // an obsolete display block surviving displays or modifier releases.
      if (dm.getDisplay(e.displayId) == null) {
        queue.removeFirst()
        continue
      }
      val mouse = mice[e.displayId]
      if (mouse == null) {
        queue.removeFirst()
        continue
      }
      if (!ready(mouse)) return
      queue.removeFirst()
      val action = e.action and MotionEvent.ACTION_MASK
      if (action == MotionEvent.ACTION_HOVER_EXIT) {
        endNativeHover(mouse, e.eventTime)
        continue
      }
      val p = e.pointers[0]
      val events = ArrayList<Triple<Int, Int, Int>>()
      if (
        capture != true &&
          (action == MotionEvent.ACTION_HOVER_ENTER ||
            action == MotionEvent.ACTION_HOVER_MOVE ||
            action == MotionEvent.ACTION_MOVE)
      ) {
        val current = position.invoke(im, e.displayId) as? PointF
        if (current == null) {
          queue.addFirst(pending)
          return
        }
        val size = Point()
        dm.getDisplay(e.displayId)?.getRealSize(size)
        val step = mouse.pointer.move(p.x, p.y, current.x, current.y, size.x, size.y)
        events += Triple(2, 0, step.dx)
        events += Triple(2, 1, step.dy)
        connection.send(0x0609) { u32(e.displayId).i32(Math.round(step.x)).i32(Math.round(step.y)) }
      }
      for ((mask, code) in listOf(1 to 272, 2 to 273, 4 to 274)) {
        if ((mouse.buttons xor e.buttonState) and mask != 0)
          events += Triple(1, code, if (e.buttonState and mask != 0) 1 else 0)
      }
      mouse.buttons = e.buttonState
      if (action == MotionEvent.ACTION_SCROLL) {
        if (p.vscroll != 0f) events += Triple(2, 11, Math.round(p.vscroll * 120))
        if (p.hscroll != 0f) events += Triple(2, 12, Math.round(p.hscroll * 120))
      }
      if (events.isNotEmpty()) {
        if (send(mouse.device, e.eventTime, events)) {
          mouse.sentButtons = e.buttonState
          mouse.nativeHover = true
        }
      }
    }
  }

  private fun endNativeHover(mouse: Mouse, time: Long) {
    // A relative mouse has no out-of-range coordinate. End this discontinuous
    // stream with evdev's resync marker; InputReader resets the device, and
    // InputDispatcher emits HOVER_EXIT under its real ID. An injected exit
    // cannot do this: Android rewrites injected device IDs to -1.
    if (mouse.nativeHover && send(mouse.device, time, listOf(Triple(0, 3, 0))))
      mouse.nativeHover = false
    mouse.pointer.reset()
  }

  // Release buttons whose UP could not be delivered across a disconnect.
  fun reset() {
    val now = SystemClock.uptimeMillis()
    handler.post {
      queue.clear()
      capture = null
      captureChanged()
      flushRemovals()
      for (mouse in mice.values) {
        val releases =
          listOf(1 to 272, 2 to 273, 4 to 274)
            .filter { (mask, _) -> mouse.sentButtons and mask != 0 }
            .map { (_, code) -> Triple(1, code, 0) }
        if (releases.isNotEmpty() && send(mouse.device, now, releases)) mouse.sentButtons = 0
        mouse.buttons = 0
        endNativeHover(mouse, now)
        connection.send(0x060a) { u32(mouse.device).u32(1) }
      }
      keyboard.sync(0) { send(0, now, it) }
      queuedMeta = 0
    }
  }

  private fun send(device: Int, time: Long, events: List<Triple<Int, Int, Int>>): Boolean =
    connection.send(0x0608) {
      u32(device).u64(time * 1000).u32(events.size)
      for ((type, code, value) in events) u16(type).u16(code).i32(value)
    }
}
