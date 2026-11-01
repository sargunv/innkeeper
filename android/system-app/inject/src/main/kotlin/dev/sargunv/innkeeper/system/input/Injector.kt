// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.input

import android.content.res.Resources
import android.view.InputDevice
import android.view.InputEvent
import android.view.KeyCharacterMap
import android.view.KeyEvent
import android.view.MotionEvent
import java.lang.reflect.Method

/**
 * Builds real `MotionEvent`/`KeyEvent`s from [OutEvent]s and injects them through the hidden
 * `InputManagerGlobal.injectInputEvent`, which needs INJECT_EVENTS. Hidden APIs are open to
 * platform-signed apps. Injection is asynchronous, so InputDispatcher queues the event and returns.
 */
class Injector {
  private val inputManager: Any =
    Class.forName("android.hardware.input.InputManagerGlobal").getMethod("getInstance").invoke(null)
      ?: error("InputManagerGlobal unavailable")
  private val inject: Method =
    inputManager.javaClass.getMethod(
      "injectInputEvent",
      InputEvent::class.java,
      Int::class.javaPrimitiveType,
    )

  /** Returns false if InputManager refused the event. */
  fun inject(e: OutEvent): Boolean {
    val event = toInputEvent(e)
    try {
      return inject.invoke(inputManager, event, INJECT_INPUT_EVENT_MODE_ASYNC) as Boolean
    } finally {
      if (event is MotionEvent) event.recycle()
    }
  }

  companion object {
    // InputManager.INJECT_INPUT_EVENT_MODE_ASYNC (hidden).
    private const val INJECT_INPUT_EVENT_MODE_ASYNC = 0

    // Hidden. Injected events go to this display's focused window or the window under the pointer.
    private val setDisplayId: Method by lazy {
      InputEvent::class.java.getMethod("setDisplayId", Int::class.javaPrimitiveType)
    }

    // Hidden (@TestApi). The public obtain() overloads have no action button.
    private val setActionButton: Method by lazy {
      MotionEvent::class.java.getMethod("setActionButton", Int::class.javaPrimitiveType)
    }

    // Hidden. The public obtain() overloads have no display, flags or classification.
    private val obtain: Method by lazy {
      val i = Int::class.javaPrimitiveType
      val l = Long::class.javaPrimitiveType
      val f = Float::class.javaPrimitiveType
      MotionEvent::class
        .java
        .getMethod(
          "obtain",
          l, // downTime
          l, // eventTime
          i, // action
          i, // pointerCount
          Array<MotionEvent.PointerProperties>::class.java,
          Array<MotionEvent.PointerCoords>::class.java,
          i, // metaState
          i, // buttonState
          f, // xPrecision
          f, // yPrecision
          i, // deviceId
          i, // edgeFlags
          i, // source
          i, // displayId
          i, // flags
          i, // classification
        )
    }

    /**
     * Display pixels per AXIS_VSCROLL unit ([TranslatorConfig.scrollPixelsPerUnit]). This is
     * `ViewConfiguration.getScaledVerticalScrollFactor()` without a UI context, which is the
     * framework's config_verticalScrollFactor (64 dp unless overlaid).
     */
    fun scrollPixelsPerUnit(): Float {
      val res = Resources.getSystem()
      val id = res.getIdentifier("config_verticalScrollFactor", "dimen", "android")
      return if (id != 0) res.getDimension(id) else 64f * res.displayMetrics.density
    }

    fun toInputEvent(e: OutEvent): InputEvent =
      when (e) {
        is OutKey ->
          KeyEvent(
              e.downTime,
              e.eventTime,
              e.action,
              e.keyCode,
              e.repeatCount, // nonzero only for a compositor's own repeats
              e.metaState,
              KeyCharacterMap.VIRTUAL_KEYBOARD,
              e.scanCode,
              e.flags,
              InputDevice.SOURCE_KEYBOARD,
            )
            .also { if (e.displayId != 0) setDisplayId.invoke(it, e.displayId) }
        is OutMotion -> toMotionEvent(e)
      }

    fun toMotionEvent(e: OutMotion): MotionEvent {
      val n = e.pointers.size
      val props = Array(n) { MotionEvent.PointerProperties() }
      val coords = Array(n) { MotionEvent.PointerCoords() }
      for ((i, p) in e.pointers.withIndex()) {
        props[i].id = p.id
        props[i].toolType = p.toolType
        coords[i].x = p.x
        coords[i].y = p.y
        coords[i].pressure = p.pressure
        coords[i].size =
          if (p.pressure > 0f && p.toolType == MotionEvent.TOOL_TYPE_FINGER) 0.1f else 0f
        if (p.vscroll != 0f) coords[i].setAxisValue(MotionEvent.AXIS_VSCROLL, p.vscroll)
        if (p.hscroll != 0f) coords[i].setAxisValue(MotionEvent.AXIS_HSCROLL, p.hscroll)
        for ((axis, v) in p.axes) coords[i].setAxisValue(axis, v)
      }
      var flags = e.flags
      if (e.action and MotionEvent.ACTION_MASK == MotionEvent.ACTION_CANCEL) {
        flags = flags or MotionEvent.FLAG_CANCELED
      }
      // obtain() sets the mouse cursor position from the pointers (InputDispatcher targets mouse
      // events by it).
      val m =
        obtain.invoke(
          null,
          e.downTime,
          e.eventTime,
          e.action,
          n,
          props,
          coords,
          e.metaState,
          e.buttonState,
          1f,
          1f,
          KeyCharacterMap.VIRTUAL_KEYBOARD,
          0,
          e.source,
          e.displayId,
          flags,
          e.classification,
        ) as MotionEvent
      if (e.actionButton != 0) setActionButton.invoke(m, e.actionButton)
      return m
    }
  }
}
