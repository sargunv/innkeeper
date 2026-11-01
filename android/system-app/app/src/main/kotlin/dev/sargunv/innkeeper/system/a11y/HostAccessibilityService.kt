// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.a11y

import android.accessibilityservice.AccessibilityService
import android.content.Intent
import android.graphics.Rect
import android.os.Bundle
import android.os.Handler
import android.os.HandlerThread
import android.text.Spanned
import android.text.style.URLSpan
import android.util.Log
import android.view.Display
import android.view.accessibility.AccessibilityEvent
import android.view.accessibility.AccessibilityNodeInfo
import android.view.accessibility.AccessibilityNodeInfo.AccessibilityAction
import android.view.accessibility.AccessibilityWindowInfo
import dev.sargunv.innkeeper.system.InnkeeperSystemApp
import dev.sargunv.innkeeper.system.a11y.A11yWire.Node

/**
 * Sends the bridge each app window's accessibility tree, with the windows on the window's virtual
 * display under one root (node 1). It sends a full snapshot when the display's windows change, and
 * partial snapshots of the changed node and its subtree as apps update. Events are coalesced for
 * [DEBOUNCE_MS], and walks and actions run on their own thread. [A11yFeature] enables the service
 * only while the desktop's assistive technologies are on. It requests no touch exploration and only
 * reads and acts.
 */
class HostAccessibilityService : AccessibilityService() {
  private val app
    get() = application as InnkeeperSystemApp

  private val thread = HandlerThread("a11y")
  private lateinit var handler: Handler

  /** Per display, keyed by the window's id: its node ids and what was last sent. [thread] only. */
  private class DisplayTree(val id: Int) {
    var title = ""
    var scale = 1f
    var windows = emptyList<Int>()
    var needsFull = true
    var focus = 0L
    var sentFocus = 0L
    var nextId = 2L
    var ids = HashMap<AccessibilityNodeInfo, Long>()
    var nodes = HashMap<Long, AccessibilityNodeInfo>()
  }

  private val displays = HashMap<Int, DisplayTree>()
  // Changes since the last flush: windows to re-send whole, changed nodes by window, focus moves.
  private var dirtyAll = false
  private val dirtyWindows = HashSet<Int>()
  private val dirtyNodes = ArrayList<AccessibilityNodeInfo>()
  private var flushQueued = false

  override fun onServiceConnected() {
    thread.start()
    handler = Handler(thread.looper)
    Log.i(TAG, "accessibility service connected")
    app.a11y.service = this
    resync()
  }

  override fun onUnbind(intent: Intent?): Boolean {
    if (app.a11y.service === this) app.a11y.service = null
    thread.quitSafely()
    Log.i(TAG, "accessibility service unbound")
    return false
  }

  override fun onInterrupt() {}

  /** Re-sends every window whole after the bridge restarts. */
  fun resync() {
    if (!thread.isAlive) return
    handler.post {
      dirtyAll = true
      for (d in displays.values) d.needsFull = true
      queueFlush()
    }
  }

  override fun onAccessibilityEvent(e: AccessibilityEvent) {
    val type = e.eventType
    if (type and RELEVANT == 0) return
    val window = e.windowId
    val source = if (type and NODE_EVENTS != 0) e.source else null
    handler.post {
      when {
        type and WINDOW_EVENTS != 0 -> if (window >= 0) dirtyWindows += window else dirtyAll = true
        source != null -> dirtyNodes += source
        window >= 0 -> dirtyWindows += window
      }
      if (type == AccessibilityEvent.TYPE_VIEW_FOCUSED && source != null) focusMoved(source)
      queueFlush()
    }
  }

  private fun focusMoved(source: AccessibilityNodeInfo) {
    for (d in displays.values) d.ids[source]?.let { d.focus = it }
  }

  private fun queueFlush() {
    if (flushQueued) return
    flushQueued = true
    handler.postDelayed(::flush, DEBOUNCE_MS)
  }

  private fun flush() {
    flushQueued = false
    if (!app.a11y.bridge.connected) return
    val all = windowsOnAllDisplays
    val onDisplays = HashMap<Int, List<AccessibilityWindowInfo>>()
    for (i in 0 until all.size()) {
      if (all.keyAt(i) != Display.DEFAULT_DISPLAY) {
        onDisplays[all.keyAt(i)] = all.valueAt(i).sortedByDescending { it.layer }
      }
    }
    fun windowsOf(d: Int) = onDisplays[d].orEmpty().map { it.id }
    // Window titles and scales come from the window manager, when windows come and go.
    if (
      dirtyAll ||
        onDisplays.keys != displays.keys ||
        displays.values.any { it.needsFull || windowsOf(it.id) != it.windows }
    ) {
      val wm = app.windows?.list().orEmpty().associateBy { it.display }
      for (gone in displays.keys - wm.keys) {
        displays.remove(gone)
        app.a11y.bridge.send(A11yWire.REMOVE) { u32(gone) }
      }
      for ((id, w) in wm) {
        val d = displays.getOrPut(id) { DisplayTree(id) }
        d.title = w.label
        d.scale = w.dpi / 160f
      }
    }
    for (d in displays.values) {
      val wins = onDisplays[d.id].orEmpty()
      val ids = wins.map { it.id }
      if (dirtyAll || d.needsFull || ids != d.windows || ids.any { it in dirtyWindows }) {
        sendFull(d, wins)
      } else {
        val changed = dirtyNodes.filter { it.windowId in ids }
        if (changed.isNotEmpty() || d.focus != d.sentFocus) sendPartial(d, changed)
      }
    }
    dirtyAll = false
    dirtyWindows.clear()
    dirtyNodes.clear()
  }

  private fun sendFull(d: DisplayTree, windows: List<AccessibilityWindowInfo>) {
    val oldIds = d.ids
    d.ids = HashMap()
    d.nodes = HashMap()
    d.windows = windows.map { it.id }
    val root = Node(1).apply { flags = A11yWire.ENABLED or A11yWire.VISIBLE_TO_USER }
    val out = arrayListOf(root)
    val seen = HashSet<AccessibilityNodeInfo>()
    var focus = 0L
    for (w in windows) {
      val r = w.getRoot(AccessibilityNodeInfo.FLAG_PREFETCH_DESCENDANTS_HYBRID) ?: continue
      root.children +=
        walk(d, r, out, seen, oldIds) { info, id ->
          if (focus == 0L && info.isFocused && w.isFocused) focus = id
        }
    }
    // The root covers the windows.
    val r = Rect()
    val all = Rect()
    for (w in windows) {
      w.getBoundsInScreen(r)
      all.union(r)
    }
    root.bounds[0] = all.left
    root.bounds[1] = all.top
    root.bounds[2] = all.right
    root.bounds[3] = all.bottom
    if (focus != 0L) d.focus = focus
    if (d.focus !in d.nodes) d.focus = 0
    d.needsFull = false
    send(d, false, out)
  }

  private fun sendPartial(d: DisplayTree, changed: List<AccessibilityNodeInfo>) {
    val out = ArrayList<Node>()
    val seen = HashSet<AccessibilityNodeInfo>()
    for (n in changed) {
      if (n in seen) continue
      if (n !in d.ids) {
        // The bridge doesn't have this node yet. Its parent changed too, but which one is unknown.
        d.needsFull = true
        queueFlush()
        return
      }
      if (!n.refresh()) continue
      walk(d, n, out, seen, d.ids) { info, id -> if (info.isFocused) d.focus = id }
    }
    send(d, true, out)
  }

  private fun send(d: DisplayTree, partial: Boolean, nodes: List<Node>) {
    val blob = A11yWire.encode(partial, d.scale, d.focus, d.title, nodes)
    if (blob.size > A11yWire.MAX_BLOB) {
      Log.w(TAG, "display ${d.id}: tree too large (${blob.size} bytes)")
      return
    }
    d.sentFocus = d.focus
    val id = d.id
    app.a11y.bridge.send(
      A11yWire.TREE,
      blob,
      onReply = { r ->
        // -ENOENT means a partial snapshot came before a full one, because the bridge restarted or
        // switched off.
        if (r.status == -ENOENT) {
          handler.post {
            displays[id]?.needsFull = true
            queueFlush()
          }
        } else if (!r.ok && r.status != -EAGAIN) {
          Log.w(TAG, "display $id: tree refused: ${r.status}")
        }
      },
    ) {
      u32(id)
      u32(blob.size)
    }
  }

  /**
   * Appends [top] and its subtree (preorder) to [out], keeping ids from [oldIds], and returns
   * [top]'s id. [visit] sees each node.
   */
  private fun walk(
    d: DisplayTree,
    top: AccessibilityNodeInfo,
    out: MutableList<Node>,
    seen: MutableSet<AccessibilityNodeInfo>,
    oldIds: Map<AccessibilityNodeInfo, Long>,
    visit: (AccessibilityNodeInfo, Long) -> Unit,
  ): Long {
    fun idOf(n: AccessibilityNodeInfo): Long =
      d.ids.getOrPut(n) { oldIds[n] ?: d.nextId++ }.also { d.nodes[it] = n }
    val topId = idOf(top)
    val stack = ArrayDeque<AccessibilityNodeInfo>()
    stack.addLast(top)
    seen += top
    while (stack.isNotEmpty() && out.size < A11yWire.MAX_NODES) {
      val info = stack.removeLast()
      val id = idOf(info)
      val node = convert(info, id)
      val kids = ArrayList<AccessibilityNodeInfo>()
      for (i in 0 until minOf(info.childCount, A11yWire.MAX_CHILDREN)) {
        val c = info.getChild(i) ?: continue
        if (!seen.add(c)) continue
        node.children += idOf(c)
        kids += c
      }
      visit(info, id)
      out += node
      for (c in kids.asReversed()) stack.addLast(c)
    }
    return topId
  }

  /** Runs an assistive technology's request on its node. */
  fun perform(a: A11yWire.Action) {
    if (!thread.isAlive) return
    handler.post {
      val info = displays[a.window]?.nodes?.get(a.node) ?: return@post
      val id = if (a.action == A11yWire.CUSTOM) a.a else ACTION_IDS[a.action] ?: return@post
      val args = Bundle()
      when (a.action) {
        A11yWire.SET_TEXT ->
          args.putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE, a.text)
        A11yWire.SET_SELECTION -> {
          args.putInt(AccessibilityNodeInfo.ACTION_ARGUMENT_SELECTION_START_INT, a.a)
          args.putInt(AccessibilityNodeInfo.ACTION_ARGUMENT_SELECTION_END_INT, a.b)
        }
        A11yWire.SET_PROGRESS ->
          args.putFloat(AccessibilityNodeInfo.ACTION_ARGUMENT_PROGRESS_VALUE, a.value)
      }
      if (!info.performAction(id, args)) Log.i(TAG, "action ${a.action} on ${a.node}: refused")
    }
  }

  /** One node in the bridge's fields (Chromium's `accessibility_helper.mojom`). */
  private fun convert(info: AccessibilityNodeInfo, id: Long): Node {
    val n = Node(id)
    var f = 0
    fun flag(on: Boolean, bit: Int) {
      if (on) f = f or bit
    }
    flag(info.isCheckable, A11yWire.CHECKABLE)
    when (info.checked) {
      AccessibilityNodeInfo.CHECKED_STATE_TRUE -> f = f or A11yWire.CHECKED
      AccessibilityNodeInfo.CHECKED_STATE_PARTIAL -> f = f or A11yWire.CHECKED_MIXED
    }
    flag(info.isFocusable, A11yWire.FOCUSABLE)
    flag(info.isFocused, A11yWire.FOCUSED)
    flag(info.isSelected, A11yWire.SELECTED)
    flag(info.isClickable, A11yWire.CLICKABLE)
    flag(info.isLongClickable, A11yWire.LONG_CLICKABLE)
    flag(info.isEnabled, A11yWire.ENABLED)
    flag(info.isPassword, A11yWire.PASSWORD)
    flag(info.isScrollable, A11yWire.SCROLLABLE)
    flag(info.isVisibleToUser, A11yWire.VISIBLE_TO_USER)
    flag(info.isEditable, A11yWire.EDITABLE)
    flag(info.isMultiLine, A11yWire.MULTI_LINE)
    flag(info.isContentInvalid, A11yWire.CONTENT_INVALID)
    flag(info.isHeading, A11yWire.HEADING)
    flag(info.isShowingHintText, A11yWire.SHOWING_HINT_TEXT)
    flag(info.isDismissable, A11yWire.DISMISSABLE)
    flag(info.isImportantForAccessibility, A11yWire.IMPORTANT)
    val text = info.text
    if (text is Spanned && text.isNotEmpty()) {
      flag(
        text.getSpans(0, text.length, URLSpan::class.java).any {
          text.getSpanStart(it) == 0 && text.getSpanEnd(it) == text.length
        },
        A11yWire.TEXT_IS_LINK,
      )
    }
    n.flags = f
    for (a in info.actionList) {
      val bit = STANDARD[a.id]
      if (bit != null) {
        n.actions = n.actions or (1 shl bit)
      } else if (a.label != null && isCustom(a.id)) {
        n.custom += a.id to a.label.toString()
      }
    }
    val r = Rect()
    info.getBoundsInScreen(r)
    n.bounds[0] = r.left
    n.bounds[1] = r.top
    n.bounds[2] = r.right
    n.bounds[3] = r.bottom
    n.className = info.className?.toString().orEmpty()
    n.text = text?.toString().orEmpty()
    n.contentDescription = info.contentDescription?.toString().orEmpty()
    n.hint = info.hintText?.toString().orEmpty()
    n.labeledBy = info.labeledBy?.let { (it.contentDescription ?: it.text)?.toString() }.orEmpty()
    n.stateDescription = info.stateDescription?.toString().orEmpty()
    n.paneTitle = info.paneTitle?.toString().orEmpty()
    n.tooltip = info.tooltipText?.toString().orEmpty()
    n.viewId = info.viewIdResourceName.orEmpty()
    n.selectionStart = info.textSelectionStart
    n.selectionEnd = info.textSelectionEnd
    n.live = info.liveRegion
    info.rangeInfo?.let {
      val kind =
        when (it.type) {
          AccessibilityNodeInfo.RangeInfo.RANGE_TYPE_INT -> 1
          AccessibilityNodeInfo.RangeInfo.RANGE_TYPE_FLOAT -> 2
          AccessibilityNodeInfo.RangeInfo.RANGE_TYPE_PERCENT -> 3
          else -> 0
        }
      if (kind != 0) n.range = A11yWire.Range(kind, it.min, it.max, it.current)
    }
    info.collectionInfo?.let {
      n.collection =
        A11yWire.Collection(it.rowCount, it.columnCount, it.isHierarchical, it.selectionMode)
    }
    info.collectionItemInfo?.let {
      @Suppress("DEPRECATION")
      n.item = A11yWire.Item(it.rowIndex, it.columnIndex, it.rowSpan, it.columnSpan, it.isHeading)
    }
    return n
  }

  companion object {
    const val TAG = A11yFeature.TAG
    const val DEBOUNCE_MS = 100L
    private const val ENOENT = 2
    private const val EAGAIN = 11

    private const val WINDOW_EVENTS =
      AccessibilityEvent.TYPE_WINDOWS_CHANGED or AccessibilityEvent.TYPE_WINDOW_STATE_CHANGED
    private const val NODE_EVENTS =
      AccessibilityEvent.TYPE_WINDOW_CONTENT_CHANGED or
        AccessibilityEvent.TYPE_VIEW_TEXT_CHANGED or
        AccessibilityEvent.TYPE_VIEW_TEXT_SELECTION_CHANGED or
        AccessibilityEvent.TYPE_VIEW_SELECTED or
        AccessibilityEvent.TYPE_VIEW_CLICKED or
        AccessibilityEvent.TYPE_VIEW_SCROLLED or
        AccessibilityEvent.TYPE_VIEW_FOCUSED
    private const val RELEVANT = WINDOW_EVENTS or NODE_EVENTS

    /** Standard actions, with their ids and bits in the bridge's action mask. */
    private val STANDARD: Map<Int, Int> =
      mapOf(
        AccessibilityNodeInfo.ACTION_CLICK to A11yWire.CLICK,
        AccessibilityNodeInfo.ACTION_LONG_CLICK to A11yWire.LONG_CLICK,
        AccessibilityNodeInfo.ACTION_FOCUS to A11yWire.FOCUS,
        AccessibilityNodeInfo.ACTION_CLEAR_FOCUS to A11yWire.CLEAR_FOCUS,
        AccessibilityNodeInfo.ACTION_SELECT to A11yWire.SELECT,
        AccessibilityNodeInfo.ACTION_SCROLL_FORWARD to A11yWire.SCROLL_FORWARD,
        AccessibilityNodeInfo.ACTION_SCROLL_BACKWARD to A11yWire.SCROLL_BACKWARD,
        AccessibilityAction.ACTION_SCROLL_UP.id to A11yWire.SCROLL_UP,
        AccessibilityAction.ACTION_SCROLL_DOWN.id to A11yWire.SCROLL_DOWN,
        AccessibilityAction.ACTION_SCROLL_LEFT.id to A11yWire.SCROLL_LEFT,
        AccessibilityAction.ACTION_SCROLL_RIGHT.id to A11yWire.SCROLL_RIGHT,
        AccessibilityNodeInfo.ACTION_EXPAND to A11yWire.EXPAND,
        AccessibilityNodeInfo.ACTION_COLLAPSE to A11yWire.COLLAPSE,
        AccessibilityNodeInfo.ACTION_DISMISS to A11yWire.DISMISS,
        AccessibilityNodeInfo.ACTION_SET_TEXT to A11yWire.SET_TEXT,
        AccessibilityNodeInfo.ACTION_SET_SELECTION to A11yWire.SET_SELECTION,
        AccessibilityAction.ACTION_SET_PROGRESS.id to A11yWire.SET_PROGRESS,
        AccessibilityAction.ACTION_SHOW_ON_SCREEN.id to A11yWire.SHOW_ON_SCREEN,
        AccessibilityAction.ACTION_CONTEXT_CLICK.id to A11yWire.CONTEXT_CLICK,
        AccessibilityAction.ACTION_SHOW_TOOLTIP.id to A11yWire.SHOW_TOOLTIP,
        AccessibilityAction.ACTION_HIDE_TOOLTIP.id to A11yWire.HIDE_TOOLTIP,
      )

    private val ACTION_IDS: Map<Int, Int> = STANDARD.entries.associate { (k, v) -> v to k }

    /**
     * Whether [id] is an app's own action. The framework's actions are single bits below 2^22 (the
     * legacy ones) or `android.R.id` values (package 0x01).
     */
    private fun isCustom(id: Int) =
      !(id > 0 && id and (id - 1) == 0 && id < (1 shl 22)) && (id ushr 24) != 0x01
  }
}
