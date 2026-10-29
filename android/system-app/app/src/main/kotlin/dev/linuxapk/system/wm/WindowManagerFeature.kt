// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.wm

import android.app.ActivityManager
import android.app.ActivityOptions
import android.app.ActivityTaskManager
import android.app.TaskStackListener
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.pm.ActivityInfo
import android.content.pm.ApplicationInfo
import android.graphics.ImageFormat
import android.hardware.HardwareBuffer
import android.hardware.display.DisplayManager
import android.hardware.display.VirtualDisplay
import android.hardware.display.VirtualDisplayConfig
import android.media.ImageReader
import android.net.Uri
import android.os.Handler
import android.os.HandlerThread
import android.os.SystemClock
import android.util.Log
import android.view.WindowInsets
import android.view.WindowManagerGlobal
import dev.linuxapk.system.Feature
import dev.linuxapk.system.SystemProps
import dev.linuxapk.system.bridge.Wire
import dev.linuxapk.system.input.HostEvent
import dev.linuxapk.system.input.HostInputClient
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.CountDownLatch
import java.util.concurrent.FutureTask
import java.util.concurrent.TimeUnit
import kotlin.math.roundToInt

/**
 * The window manager (docs/display.md). Each app task gets its own trusted virtual display, and the
 * bridge shows each display as its own Wayland window:
 * 1. [launch] creates the display without a surface, asks the bridge for a window
 *    ([HostInputClient.requestWindow]), then gives the display its ImageReader surface.
 *    SurfaceFlinger then creates the HWC virtual display, which gets the requested window.
 * 2. The app starts there (`ActivityOptions.setLaunchDisplayId`).
 * 3. Window events: a resize (`window`) resizes the display to the window's logical size times its
 *    scale, at 160 dpi times the scale; `close` removes the tasks and releases the display;
 *    `keyboard_enter` focuses the display's top task.
 * 4. A task stack listener closes the window when the display's last task is gone, and moves any
 *    app task that shows up on display 0 into a window of its own.
 *
 * A launch may carry the desktop's activation token (docs/display.md, "Launch feedback"): a new
 * window uses it on its first frame, a shown one when it is focused.
 *
 * Everything runs on one handler thread; public methods post to it.
 */
class WindowManagerFeature(
  private val context: Context,
  private val input: HostInputClient,
) : Feature, HostInputClient.Listener {
  override val name = "windows"

  private val thread = HandlerThread("linuxapk-wm")
  private lateinit var handler: Handler
  private val frames = HandlerThread("linuxapk-frames")
  private lateinit var frameHandler: Handler
  private val atm by lazy { ActivityTaskManager.getService() }
  private val wms by lazy { WindowManagerGlobal.getWindowManagerService() }
  private val dm by lazy { context.getSystemService(DisplayManager::class.java) }
  private val prefs by lazy {
    context.createDeviceProtectedStorageContext().getSharedPreferences("wm", Context.MODE_PRIVATE)
  }

  private val windows = LinkedHashMap<Int, AppWindow>() // by display id
  // The scale new windows start at: the last one a window reported (kept across restarts, so the
  // first window after boot opens at the desktop's scale).
  private var scale = prefs.getFloat(PREF_SCALE, 1f).toDouble().takeIf { it in 0.5..8.0 } ?: 1.0
  private var refreshQueued = false
  private val changeListeners = CopyOnWriteArrayList<() -> Unit>()

  /** Packages with a window, shown or starting (any thread). */
  @Volatile
  var packages: Set<String> = emptySet()
    private set

  /** Calls [l] on the window manager's thread whenever a window opens or closes. */
  fun addChangeListener(l: () -> Unit) {
    changeListeners += l
  }

  private fun changed() {
    packages = windows.values.mapTo(HashSet()) { it.pkg }
    changeListeners.forEach { it() }
  }

  private class AppWindow(
    val pkg: String,
    val label: String,
    val display: VirtualDisplay,
    val reader: ImageReader,
    var width: Int,
    var height: Int,
    var dpi: Int,
    val sizing: WindowSizing,
    /** The launch's activation token, until the window is requested with it. */
    var token: String,
  ) {
    val id: Int = display.display.displayId
    /** Counted down once the window shows its first frame. */
    val shown = CountDownLatch(1)
    val created = SystemClock.uptimeMillis()
    var tasks: List<Int> = emptyList()
    var hadTask = false
    var mapped = false
    var resize: Runnable? = null
    var appearance: Runnable? = null
    var topTask: Int? = null
    var lastColor: Int? = null
    var lastIcon: ByteArray? = null
    var requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_UNSPECIFIED
    var topActivity: ComponentName? = null
    val orientations = HashMap<Pair<Int, ComponentName>, Int>()
    var requestedVisibleTypes = WindowInsets.Type.systemBars()
    var insetComponent: ComponentName? = null
    var game = false
  }

  /** One row of [list]. */
  data class WindowInfo(
    val display: Int,
    val pkg: String,
    val label: String,
    val width: Int,
    val height: Int,
    val dpi: Int,
    val tasks: List<Int>,
    val mapped: Boolean,
  ) {
    override fun toString() =
      "display=$display package=$pkg size=${width}x$height dpi=$dpi " +
        "tasks=${tasks.joinToString(",").ifEmpty { "-" }} window=${if (mapped) "yes" else "no"} " +
        "label=$label"
  }

  override fun start() {
    thread.start()
    handler = Handler(thread.looper)
    frames.start()
    frameHandler = Handler(frames.looper)
    input.addListener(this)
    atm.registerTaskStackListener(taskListener)
    // AppWarnings shows these on the default display, which has no window; AppWarningActivity
    // shows the same warning in the app's own window instead.
    SystemProps.set("debug.wm.disable_deprecated_target_sdk_dialog", "true")
    Log.i(TAG, "window manager started")
    handler.post { refreshTasks() }
  }

  override fun stop() {
    atm.unregisterTaskStackListener(taskListener)
    onWm { windows.values.toList().forEach { close(it, finishTasks = false) } }
    thread.quitSafely()
    frames.quitSafely()
  }

  // ---- control API (ControlReceiver) ---------------------------------------

  data class LaunchRequest(
    val pkg: String,
    val component: ComponentName? = null,
    val uri: Uri? = null,
    val widthDp: Int = 0,
    val heightDp: Int = 0,
    /** The desktop's `xdg_activation_v1` token for this launch (empty: none). */
    val token: String = "",
    val file: Boolean = false,
    /** An explicit file handoff prepared by the system app (VIEW or SEND). */
    val launchIntent: Intent? = null,
  )

  /** Opens [r]'s app in a new window, or focuses its window. Returns a one-line result. */
  fun launch(r: LaunchRequest): String = onWm { launchLocked(r) }

  /**
   * Waits up to [timeoutMs] for the window of [display] to show its first frame. Returns false on
   * timeout or if it has no window. Not on the window manager's thread.
   */
  fun awaitFirstFrame(display: Int, timeoutMs: Long): Boolean {
    val latch = onWm { windows[display]?.shown } ?: return false
    return latch.await(timeoutMs, TimeUnit.MILLISECONDS)
  }

  fun list(): List<WindowInfo> = onWm {
    refreshTasks()
    windows.values.map {
      WindowInfo(it.id, it.pkg, it.label, it.width, it.height, it.dpi, it.tasks, it.mapped)
    }
  }

  /** Closes the window of [display] (or of [pkg]): finishes its tasks, releases the display. */
  fun close(display: Int?, pkg: String?): String = onWm {
    val w =
      windows.values.firstOrNull { (display != null && it.id == display) || it.pkg == pkg }
        ?: return@onWm "error: no such window"
    close(w, finishTasks = true)
    "closed display=${w.id} package=${w.pkg}"
  }

  private fun <T> onWm(f: () -> T): T {
    if (Thread.currentThread() == thread) return f()
    val task = FutureTask(f)
    handler.post(task)
    return task.get(20, TimeUnit.SECONDS)
  }

  // ---- launching -------------------------------------------------------------

  private fun launchLocked(r: LaunchRequest): String {
    val pm = context.packageManager
    val label =
      try {
        pm.getApplicationInfo(r.pkg, 0).loadLabel(pm).toString()
      } catch (e: Exception) {
        return "error: unknown package ${r.pkg}"
      }
    val intent =
      when {
        r.launchIntent != null -> Intent(r.launchIntent)
        r.uri != null ->
          Intent(Intent.ACTION_VIEW, r.uri).apply {
            if (r.file) {
              setDataAndType(r.uri, context.contentResolver.getType(r.uri))
              addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
            }
            if (r.component != null) component = r.component else setPackage(r.pkg)
          }
        r.component != null ->
          Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER).setComponent(r.component)
        else -> pm.getLaunchIntentForPackage(r.pkg) ?: return "error: no launcher activity"
      }
    // NEW_TASK | RESET_TASK_IF_NEEDED, as a launcher does: an existing task comes to the front.
    intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_RESET_TASK_IF_NEEDED)
    // A desktop link is an untrusted-origin VIEW: only activities that accept web links.
    if (r.uri != null && !r.file) intent.addCategory(Intent.CATEGORY_BROWSABLE)

    refreshTasks()
    windows.values
      .firstOrNull { it.pkg == r.pkg }
      ?.let { w ->
        // A link or another activity opens in the app's window; its launcher activity just
        // brings the task back.
        if (r.launchIntent != null || r.uri != null || r.component != null) start(intent, w.id)
        focus(w, activate = true, token = r.token)
        return "focused display=${w.id} package=${r.pkg}"
      }

    val sizing =
      if (r.widthDp > 0 && r.heightDp > 0) WindowSizing(r.widthDp, r.heightDp, 0, 0)
      else sizingFor(intent)
    val w = createWindow(r.pkg, label, sizing, r.token) ?: return "error: no virtual display"
    // An existing task (say, started on display 0) moves over instead of starting anew.
    val existing = tasksOf(r.pkg).firstOrNull()
    if (existing != null && r.uri == null && r.launchIntent == null) {
      move(existing, w)
    } else {
      start(intent, w.id)
    }
    return "launched display=${w.id} package=${r.pkg} size=${w.width}x${w.height} dpi=${w.dpi}"
  }

  private fun start(intent: Intent, display: Int) {
    val opts = ActivityOptions.makeBasic().setLaunchDisplayId(display)
    context.startActivity(intent, opts.toBundle())
  }

  private fun move(task: ActivityManager.RunningTaskInfo, w: AppWindow) {
    Log.i(TAG, "moving task ${task.taskId} (${task.baseActivity}) to display ${w.id}")
    atm.moveRootTaskToDisplay(task.taskId, w.id)
    atm.focusTopTask(w.id)
  }

  private fun sizingFor(intent: Intent): WindowSizing {
    val ai =
      try {
        context.packageManager.resolveActivity(intent, 0)?.activityInfo
      } catch (e: Exception) {
        null
      }
    return WindowSizing.of(ai)
  }

  private fun createWindow(
    pkg: String,
    label: String,
    sizing: WindowSizing,
    token: String = "",
  ): AppWindow? {
    val s = scale
    val width = (sizing.widthDp * s).roundToInt()
    val height = (sizing.heightDp * s).roundToInt()
    val dpi = (BASE_DPI * s).roundToInt()
    // SurfaceFlinger renders each frame into one of these buffers and hands the same buffer to
    // the composer, which copies it into the window; the frames themselves are dropped here.
    val reader =
      ImageReader.newInstance(
        width,
        height,
        ImageFormat.PRIVATE,
        3,
        HardwareBuffer.USAGE_GPU_SAMPLED_IMAGE or HardwareBuffer.USAGE_COMPOSER_OVERLAY,
      )
    reader.setOnImageAvailableListener(
      { r ->
        try {
          r.acquireLatestImage()?.close()
        } catch (e: IllegalStateException) {
          Log.w(TAG, "frame: ${e.message}")
        }
      },
      frameHandler,
    )
    val config =
      VirtualDisplayConfig.Builder("linuxapk:$pkg", width, height, dpi).setFlags(FLAGS).build()
    val vd =
      try {
        dm.createVirtualDisplay(config, handler, null)
      } catch (e: Exception) {
        Log.e(TAG, "createVirtualDisplay for $pkg failed", e)
        null
      }
    if (vd == null) {
      reader.close()
      return null
    }
    val w = AppWindow(pkg, label, vd, reader, width, height, dpi, sizing, token)
    val ai = context.packageManager.getApplicationInfo(pkg, 0)
    @Suppress("DEPRECATION")
    w.game =
      ai.category == ApplicationInfo.CATEGORY_GAME || ai.flags and ApplicationInfo.FLAG_IS_GAME != 0
    windows[w.id] = w
    changed()
    try {
      wms.setDisplayImePolicy(w.id, DISPLAY_IME_POLICY_LOCAL)
      // The host window chooses display geometry. WMS letterboxes an activity that cannot fit
      // that orientation instead of rotating our output buffer behind the compositor's back.
      wms.setIgnoreOrientationRequest(w.id, true)
      wms.setDisplayWindowInsetsController(
        w.id,
        RequestedInsets { component, types ->
          handler.post {
            if (windows[w.id] === w) {
              w.requestedVisibleTypes = types
              w.insetComponent = component
              sendPolicy(w)
            }
          }
        },
      )
    } catch (e: Exception) {
      Log.w(TAG, "setDisplayImePolicy(${w.id}): $e")
    }
    attachSurface(w)
    Log.i(TAG, "window for $pkg: display ${w.id}, ${width}x$height @ $dpi dpi")
    return w
  }

  // Asks the bridge for the window, then gives the display its surface: SurfaceFlinger creates
  // the HWC virtual display at that point, and the bridge shows it in the requested window.
  private fun attachSurface(w: AppWindow) {
    input.requestWindow(
      w.id,
      w.width,
      w.height,
      w.dpi,
      w.sizing.minWidthDp,
      w.sizing.minHeightDp,
      APP_ID_PREFIX + w.pkg,
      w.label,
      w.token,
    )
    w.token = "" // used once
    w.display.surface = w.reader.surface
  }

  // ---- closing -----------------------------------------------------------------

  private fun close(w: AppWindow, finishTasks: Boolean) {
    if (windows.remove(w.id) == null) return
    Log.i(TAG, "closing display ${w.id} (${w.pkg}), finishTasks=$finishTasks")
    changed()
    w.appearance?.let(handler::removeCallbacks)
    w.resize?.let(handler::removeCallbacks)
    if (finishTasks) {
      for (t in w.tasks) {
        try {
          atm.removeTask(t)
        } catch (e: Exception) {
          Log.w(TAG, "removeTask($t): $e")
        }
      }
    }
    input.destroyWindow(w.id)
    // DESTROY_CONTENT_ON_REMOVAL: whatever is left on the display goes with it.
    wms.setDisplayWindowInsetsController(w.id, null)
    w.display.release()
    w.reader.close()
  }

  // ---- window events -------------------------------------------------------------

  // On the input connection's thread.
  override fun onHostEvent(e: HostEvent) {
    when (e) {
      is HostEvent.Window,
      is HostEvent.Close,
      is HostEvent.KeyboardEnter,
      is HostEvent.WindowMapped,
      is HostEvent.WindowUnmapped -> handler.post { onWindowEvent(e) }
      else -> {}
    }
  }

  // A new connection (the bridge restarted, without our windows): ask for them again.
  override fun onConnected() {
    handler.post { windows.values.forEach(::remap) }
  }

  private fun onWindowEvent(e: HostEvent) {
    when (e) {
      is HostEvent.Window -> {
        val w = windows[e.display] ?: return
        if (e.scale != scale && e.scale in 0.5..8.0) {
          scale = e.scale
          prefs.edit().putFloat(PREF_SCALE, e.scale.toFloat()).apply()
        }
        // Coalesce interactive resizes; each one is a configuration change for the app.
        w.resize?.let(handler::removeCallbacks)
        val r = Runnable {
          w.resize = null
          if (windows[w.id] === w) applySize(w, e.width, e.height, e.scale)
        }
        w.resize = r
        handler.postDelayed(r, SIZE_DEBOUNCE_MS)
      }
      is HostEvent.Close -> windows[e.display]?.let { close(it, finishTasks = true) }
      is HostEvent.KeyboardEnter -> windows[e.display]?.let { focus(it, activate = false) }
      is HostEvent.WindowMapped ->
        windows[e.display]?.let {
          it.mapped = true
          it.shown.countDown()
          sendPolicy(it)
          it.lastColor = null
          it.lastIcon = null
          queueAppearance(it)
        }
      is HostEvent.WindowUnmapped -> {
        // Its HWC display went away, or the composer did (closed windows are gone from [windows]).
        val w = windows[e.display] ?: return
        w.mapped = false
        Log.w(TAG, "window of display ${w.id} disappeared; requesting it again")
        remap(w)
      }
      else -> {}
    }
  }

  // Detaching and re-attaching the surface makes SurfaceFlinger create a new HWC virtual display,
  // which picks up the new window request.
  private fun remap(w: AppWindow) {
    if (windows[w.id] !== w) return
    w.display.surface = null
    attachSurface(w)
  }

  private fun applySize(w: AppWindow, widthDp: Int, heightDp: Int, s: Double) {
    val width = (widthDp * s).roundToInt().coerceAtLeast(1)
    val height = (heightDp * s).roundToInt().coerceAtLeast(1)
    val dpi = (BASE_DPI * s).roundToInt().coerceAtLeast(1)
    if (width == w.width && height == w.height && dpi == w.dpi) return
    Log.i(TAG, "display ${w.id}: ${w.width}x${w.height}@${w.dpi} -> ${width}x$height@$dpi")
    w.width = width
    w.height = height
    w.dpi = dpi
    w.display.resize(width, height, dpi)
  }

  // Coalesce TaskDescription churn before loading/copying a bitmap or creating a memfd.
  private fun queueAppearance(w: AppWindow) {
    w.appearance?.let(handler::removeCallbacks)
    val r = Runnable {
      w.appearance = null
      if (windows[w.id] !== w || !w.mapped) return@Runnable
      val top =
        allTasks().firstOrNull {
          Tasks.displayId(it) == w.id &&
            it.topActivity?.className != AppWarningActivity::class.java.name
        } ?: return@Runnable
      val description = top.taskDescription
      try {
        val color = description?.primaryColor ?: 0
        val icon =
          Wire.Writer().image(description?.let { Tasks.descriptionIcon(context, it) }).bytes()
        if (w.lastColor != color || w.lastIcon?.contentEquals(icon) != true) {
          if (input.windowAppearance(w.id, color, icon)) {
            w.lastColor = color
            w.lastIcon = icon
          }
        }
      } catch (e: Exception) {
        Log.w(TAG, "task appearance for display ${w.id}: $e")
      }
    }
    w.appearance = r
    handler.postDelayed(r, 120)
  }

  private fun sendPolicy(w: AppWindow) {
    if (!w.mapped) return
    val orientation =
      when {
        WindowSizing.isPortrait(w.requestedOrientation) -> 1
        WindowSizing.isLandscape(w.requestedOrientation) -> 2
        else -> 0
      }
    val sensorOrientation =
      w.requestedOrientation in
        intArrayOf(
          ActivityInfo.SCREEN_ORIENTATION_SENSOR,
          ActivityInfo.SCREEN_ORIENTATION_FULL_SENSOR,
          ActivityInfo.SCREEN_ORIENTATION_USER,
          ActivityInfo.SCREEN_ORIENTATION_FULL_USER,
        )
    val bars = WindowInsets.Type.statusBars() or WindowInsets.Type.navigationBars()
    val immersive = w.requestedVisibleTypes and bars == 0
    val fullscreen =
      w.game &&
        w.insetComponent?.packageName == w.pkg &&
        immersive &&
        (orientation == 2 || sensorOrientation)
    input.windowPolicy(w.id, orientation, fullscreen, immersive)
  }

  private fun focus(w: AppWindow, activate: Boolean, token: String = "") {
    try {
      atm.focusTopTask(w.id)
    } catch (e: Exception) {
      Log.w(TAG, "focusTopTask(${w.id}): $e")
    }
    if (activate) input.activateWindow(w.id, token)
  }

  // ---- tasks ---------------------------------------------------------------------

  private val taskListener =
    object : TaskStackListener() {
      override fun onActivityRequestedOrientationChanged(taskId: Int, requestedOrientation: Int) {
        handler.post {
          val t = allTasks().firstOrNull { it.taskId == taskId } ?: return@post
          val w = windows[Tasks.displayId(t)] ?: return@post
          if (allTasks().firstOrNull { Tasks.displayId(it) == w.id }?.taskId != taskId) return@post
          w.topActivity = t.topActivity
          t.topActivity?.let { w.orientations[taskId to it] = requestedOrientation }
          w.requestedOrientation = requestedOrientation
          Log.i(TAG, "display ${w.id}: requested orientation=$requestedOrientation task=$taskId")
          sendPolicy(w)
        }
      }

      override fun onTaskDescriptionChanged(taskInfo: ActivityManager.RunningTaskInfo?) {
        if (taskInfo == null) return
        handler.post {
          windows[Tasks.displayId(taskInfo)]?.let { w ->
            if (w.topTask == taskInfo.taskId) queueAppearance(w)
          }
        }
      }

      override fun onTaskStackChanged() = queueRefresh()

      override fun onTaskCreated(taskId: Int, componentName: ComponentName?) = queueRefresh()

      override fun onTaskRemoved(taskId: Int) = queueRefresh()

      override fun onTaskMovedToBack(taskInfo: ActivityManager.RunningTaskInfo?) {
        if (taskInfo == null) return
        handler.post {
          windows[Tasks.displayId(taskInfo)]?.let { w ->
            // Back on a launcher root backgrounds the task rather than finishing it.
            // Keep the display alive so launching it again restores the same task.
            if (w.tasks.firstOrNull() == taskInfo.taskId) input.minimizeWindow(w.id)
          }
        }
      }

      override fun onTaskMovedToFront(taskInfo: ActivityManager.RunningTaskInfo?) = queueRefresh()

      override fun onTaskDisplayChanged(taskId: Int, newDisplayId: Int) = queueRefresh()
    }

  private fun queueRefresh() {
    handler.post {
      if (!refreshQueued) {
        refreshQueued = true
        handler.postDelayed(
          {
            refreshQueued = false
            refreshTasks()
          },
          50,
        )
      }
    }
  }

  private fun tasksOf(pkg: String) =
    allTasks().filter { (it.baseActivity ?: it.topActivity)?.packageName == pkg }

  private fun allTasks(): List<ActivityManager.RunningTaskInfo> =
    try {
      atm.getTasks(MAX_TASKS, false, false, INVALID_DISPLAY)
    } catch (e: Exception) {
      Log.w(TAG, "getTasks: $e")
      emptyList()
    }

  // Syncs windows with the tasks on their displays: closes windows whose app is gone, shows
  // app warnings once a window has its task, and adopts app tasks from display 0.
  private fun refreshTasks() {
    val tasks = allTasks()
    val byDisplay = tasks.groupBy(Tasks::displayId)
    val now = SystemClock.uptimeMillis()
    for (w in windows.values.toList()) {
      val ts =
        byDisplay[w.id].orEmpty().filter {
          it.topActivity?.className != AppWarningActivity::class.java.name
        }
      w.tasks = ts.map { it.taskId }
      w.orientations.keys.retainAll { it.first in w.tasks }
      val top = ts.firstOrNull()
      if (top?.taskId != w.topTask) {
        w.topTask = top?.taskId
        queueAppearance(w)
      }
      if (top?.topActivity != w.topActivity) {
        w.topActivity = top?.topActivity
        w.requestedOrientation =
          top?.let { t ->
            t.topActivity?.let { w.orientations[t.taskId to it] }
              ?: Tasks.topActivityInfo(t)?.screenOrientation
          } ?: ActivityInfo.SCREEN_ORIENTATION_UNSPECIFIED
        sendPolicy(w)
      }
      when {
        ts.isNotEmpty() && !w.hadTask -> {
          w.hadTask = true
          AppWarningActivity.showIfNeeded(context, prefs, w.pkg, w.id)
        }
        ts.isEmpty() && w.hadTask -> close(w, finishTasks = false) // the app finished
        ts.isEmpty() && now - w.created > LAUNCH_TIMEOUT_MS -> {
          Log.w(TAG, "display ${w.id}: ${w.pkg} never started a task")
          close(w, finishTasks = false)
        }
      }
    }
    for (t in byDisplay[DEFAULT_DISPLAY].orEmpty()) {
      if (!Tasks.isAppTask(t)) continue
      val pkg = (t.baseActivity ?: t.topActivity)?.packageName ?: continue
      // Our own activities on display 0 (the desktop browser stand-in) finish at once and never
      // need a window.
      if (pkg == context.packageName) continue
      val w =
        windows.values.firstOrNull { it.pkg == pkg }
          ?: createWindow(pkg, labelOf(pkg), sizingFor(t.baseIntent ?: Intent()))
          ?: continue
      move(t, w)
    }
  }

  private fun labelOf(pkg: String): String {
    val pm = context.packageManager
    return try {
      pm.getApplicationInfo(pkg, 0).loadLabel(pm).toString()
    } catch (e: Exception) {
      pkg
    }
  }

  companion object {
    const val TAG = "LinuxApkWm"
    const val APP_ID_PREFIX = "dev.linuxapk."
    private const val PREF_SCALE = "scale"
    private const val BASE_DPI = 160
    private const val DEFAULT_DISPLAY = 0
    private const val INVALID_DISPLAY = -1
    private const val MAX_TASKS = 200
    private const val SIZE_DEBOUNCE_MS = 120L
    private const val LAUNCH_TIMEOUT_MS = 20_000L

    // WindowManager.DISPLAY_IME_POLICY_LOCAL: the input method binds to editors in the window. The
    // default one, the Linux.apk Keyboard (ime/), draws nothing; an Android IME the user picks
    // shows inside the window. HIDE would keep any IME from binding (IMMS returns NO_IME).
    private const val DISPLAY_IME_POLICY_LOCAL = 0

    // DisplayManager.VIRTUAL_DISPLAY_FLAG_* (several are @SystemApi or hidden).
    private const val FLAG_SECURE = 1 shl 2 // CAPTURE_SECURE_VIDEO_OUTPUT: FLAG_SECURE windows show
    private const val FLAG_OWN_CONTENT_ONLY = 1 shl 3 // no mirroring of display 0
    private const val FLAG_SUPPORTS_TOUCH = 1 shl 6
    private const val FLAG_DESTROY_CONTENT_ON_REMOVAL = 1 shl 8
    private const val FLAG_TRUSTED = 1 shl 10 // ADD_TRUSTED_DISPLAY
    private const val FLAG_OWN_DISPLAY_GROUP = 1 shl 11 // ADD_TRUSTED_DISPLAY
    private const val FLAG_ALWAYS_UNLOCKED = 1 shl 12 // ADD_ALWAYS_UNLOCKED_DISPLAY
    private const val FLAG_OWN_FOCUS = 1 shl 14 // per-display focus (needs TRUSTED)
    const val FLAGS =
      FLAG_SECURE or
        FLAG_OWN_CONTENT_ONLY or
        FLAG_SUPPORTS_TOUCH or
        FLAG_DESTROY_CONTENT_ON_REMOVAL or
        FLAG_TRUSTED or
        FLAG_OWN_DISPLAY_GROUP or
        FLAG_ALWAYS_UNLOCKED or
        FLAG_OWN_FOCUS
  }
}
