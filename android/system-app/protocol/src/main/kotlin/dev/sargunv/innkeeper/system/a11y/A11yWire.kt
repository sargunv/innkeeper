// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.a11y

import dev.sargunv.innkeeper.system.bridge.Wire
import java.nio.ByteBuffer

/**
 * The bridge's accessibility service (id 7, host/bridge/src/a11y/proto.rs) and its snapshot blob
 * (host/bridge/src/a11y/wire.rs). A snapshot holds one window's AccessibilityNodeInfo tree in the
 * field set of Chromium's ARC++ `accessibility_helper.mojom`. Encoding clamps every string, count
 * and float to what the bridge accepts, so the bridge never refuses a snapshot for its size. Both
 * sides test against `host/bridge/src/a11y/snapshot-fixture.txt`.
 */
object A11yWire {
  const val SERVICE = 7

  // Requests.
  const val START = 0x0701
  const val TREE = 0x0702
  const val REMOVE = 0x0703

  // Events.
  const val EV_ACTIVE = 0x4701
  const val EV_ACTION = 0x4702

  const val VERSION = 1
  const val FLAG_PARTIAL = 1

  // Node flags (wire::flag).
  const val CHECKABLE = 1 shl 0
  const val CHECKED = 1 shl 1
  const val FOCUSABLE = 1 shl 2
  const val FOCUSED = 1 shl 3
  const val SELECTED = 1 shl 4
  const val CLICKABLE = 1 shl 5
  const val LONG_CLICKABLE = 1 shl 6
  const val ENABLED = 1 shl 7
  const val PASSWORD = 1 shl 8
  const val SCROLLABLE = 1 shl 9
  const val VISIBLE_TO_USER = 1 shl 10
  const val EDITABLE = 1 shl 11
  const val MULTI_LINE = 1 shl 12
  const val CONTENT_INVALID = 1 shl 13
  const val HEADING = 1 shl 14
  const val SHOWING_HINT_TEXT = 1 shl 15
  const val DISMISSABLE = 1 shl 16
  const val TEXT_IS_LINK = 1 shl 17
  const val IMPORTANT = 1 shl 18
  const val CHECKED_MIXED = 1 shl 19

  // Actions (wire::action) are bit indexes in a node's action mask and the action of ACTION events.
  const val CLICK = 0
  const val LONG_CLICK = 1
  const val FOCUS = 2
  const val CLEAR_FOCUS = 3
  const val SELECT = 4
  const val SCROLL_FORWARD = 5
  const val SCROLL_BACKWARD = 6
  const val SCROLL_UP = 7
  const val SCROLL_DOWN = 8
  const val SCROLL_LEFT = 9
  const val SCROLL_RIGHT = 10
  const val EXPAND = 11
  const val COLLAPSE = 12
  const val DISMISS = 13
  const val SET_TEXT = 14
  const val SET_SELECTION = 15
  const val SET_PROGRESS = 16
  const val SHOW_ON_SCREEN = 17
  const val CONTEXT_CLICK = 18
  const val SHOW_TOOLTIP = 19
  const val HIDE_TOOLTIP = 20
  const val CUSTOM = 31

  // Limits (wire::limits, proto::limits).
  const val MAX_NODES = 50_000
  const val MAX_CHILDREN = 10_000
  const val MAX_CUSTOM = 32
  const val MAX_TITLE = 256
  const val MAX_CLASS = 256
  const val MAX_TEXT = 16384
  const val MAX_LONG = 4096
  const val MAX_SHORT = 1024
  const val MAX_BLOB = 16 shl 20
  /** Node ids range from 1 up to this value. The bridge's own ids are above it. */
  const val MAX_ID = 1L shl 62

  class Range(val kind: Int, val min: Float, val max: Float, val current: Float)

  class Collection(
    val rows: Int,
    val columns: Int,
    val hierarchical: Boolean,
    val selectionMode: Int,
  )

  class Item(
    val row: Int,
    val column: Int,
    val rowSpan: Int,
    val columnSpan: Int,
    val heading: Boolean,
  )

  /** One AccessibilityNodeInfo in the bridge's fields. [bounds] is left, top, right, bottom. */
  class Node(val id: Long) {
    var flags = 0
    var actions = 0
    val bounds = IntArray(4)
    var className = ""
    var text = ""
    var contentDescription = ""
    var hint = ""
    var labeledBy = ""
    var stateDescription = ""
    var paneTitle = ""
    var tooltip = ""
    var viewId = ""
    var selectionStart = -1
    var selectionEnd = -1
    var live = 0
    var range: Range? = null
    var collection: Collection? = null
    var item: Item? = null
    val custom = mutableListOf<Pair<Int, String>>()
    val children = mutableListOf<Long>()
  }

  /**
   * A snapshot blob. A full snapshot's first node is the window's root, and a partial one re-sends
   * changed nodes with their children. [focus] is the node with input focus. It is 0 for none, or
   * for unchanged in a partial snapshot. [scale] is display pixels per logical pixel.
   */
  fun encode(
    partial: Boolean,
    scale: Float,
    focus: Long,
    title: String,
    nodes: List<Node>,
  ): ByteArray {
    val w = Wire.Writer(256 + nodes.size * 96)
    val count = minOf(nodes.size, MAX_NODES)
    w.u16(VERSION).u16(if (partial) FLAG_PARTIAL else 0)
    w.f32(if (scale.isFinite()) scale.coerceIn(0.25f, 16f) else 1f)
    w.u64(if (focus in 0 until MAX_ID) focus else 0)
    str(w, title, MAX_TITLE)
    w.u32(count)
    for (n in nodes.subList(0, count)) {
      w.u64(n.id).u32(n.flags).u32(n.actions)
      n.bounds.forEach { w.i32(it) }
      str(w, n.className, MAX_CLASS)
      str(w, n.text, MAX_TEXT)
      str(w, n.contentDescription, MAX_LONG)
      str(w, n.hint, MAX_LONG)
      str(w, n.labeledBy, MAX_LONG)
      str(w, n.stateDescription, MAX_SHORT)
      str(w, n.paneTitle, MAX_SHORT)
      str(w, n.tooltip, MAX_SHORT)
      str(w, n.viewId, MAX_CLASS)
      w.i32(n.selectionStart).i32(n.selectionEnd).u8(n.live.coerceIn(0, 2))
      val g = n.range
      if (g == null || !(g.min.isFinite() && g.max.isFinite() && g.current.isFinite())) {
        w.u8(0)
      } else {
        w.u8(g.kind).f32(g.min).f32(g.max).f32(g.current)
      }
      val c = n.collection
      if (c == null) {
        w.u8(0)
      } else {
        w.u8(1).i32(c.rows).i32(c.columns).u8(if (c.hierarchical) 1 else 0).u8(c.selectionMode)
      }
      val i = n.item
      if (i == null) {
        w.u8(0)
      } else {
        w.u8(1).i32(i.row).i32(i.column).i32(i.rowSpan).i32(i.columnSpan)
        w.u8(if (i.heading) 1 else 0)
      }
      val custom = n.custom.take(MAX_CUSTOM)
      w.u16(custom.size)
      for ((id, label) in custom) {
        w.i32(id)
        str(w, label, MAX_CLASS)
      }
      val children = n.children.take(MAX_CHILDREN)
      w.u32(children.size)
      children.forEach { w.u64(it) }
    }
    return w.bytes()
  }

  /**
   * A string field without NUL, which D-Bus strings can't hold, and at most [max] bytes of UTF-8.
   */
  private fun str(w: Wire.Writer, s: String, max: Int) {
    w.line(if (s.indexOf('\u0000') >= 0) s.replace("\u0000", "") else s, max)
  }

  /** What an assistive technology asked a node to do (ACTION). */
  data class Action(
    val window: Int,
    val node: Long,
    val action: Int,
    val a: Int,
    val b: Int,
    val value: Float,
    val text: String,
  )

  fun action(r: ByteBuffer): Action {
    val window = r.int
    val node = r.long
    val action = r.get().toInt() and 0xff
    r.get()
    r.short
    val a = r.int
    val b = r.int
    val value = r.float
    val n = r.short.toInt() and 0xffff
    require(n <= r.remaining()) { "short ACTION" }
    val t = ByteArray(n)
    r.get(t)
    return Action(window, node, action, a, b, value, String(t, Charsets.UTF_8))
  }
}
