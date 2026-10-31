// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.integration.media

import android.content.Context
import android.graphics.Bitmap
import android.media.MediaMetadata
import android.media.session.MediaController
import android.media.session.MediaSessionManager
import android.media.session.PlaybackState
import android.os.SystemClock
import android.util.Log
import dev.sargunv.innkeeper.system.InnkeeperSystemApp
import dev.sargunv.innkeeper.system.bridge.Wire
import dev.sargunv.innkeeper.system.bridge.toRgba
import dev.sargunv.innkeeper.system.integration.IntegrationFeature
import dev.sargunv.innkeeper.system.wm.WindowManagerFeature

/**
 * Active media sessions as desktop MPRIS players (host/bridge/src/system/mpris.rs): each session's
 * metadata and playback state go to the bridge as `MEDIA`, and MPRIS calls (media keys, the
 * desktop's media widget) come back as transport controls. Runs on the integration thread. Needs
 * the notification listener enabled (or MEDIA_CONTENT_CONTROL) to see other apps' sessions.
 */
class MediaSync(private val context: Context, private val feature: IntegrationFeature) {
  private inner class Player(val id: Int, val controller: MediaController) :
    MediaController.Callback() {
    var lastKey: String? = null
    // What the bridge was last told, to tell a seek from normal progress.
    var sentPosition = -1L
    var sentAt = 0L
    var sentPlaying = false
    var sentSpeed = 1f

    /** Where the bridge thinks this player is now (microseconds), or -1. */
    fun expectedPosition(): Long {
      if (sentPosition < 0) return -1
      if (!sentPlaying) return sentPosition
      return sentPosition + ((SystemClock.elapsedRealtime() - sentAt) * sentSpeed * 1000).toLong()
    }

    override fun onMetadataChanged(metadata: MediaMetadata?) = send(this)

    override fun onPlaybackStateChanged(state: PlaybackState?) = send(this)

    override fun onSessionDestroyed() {
      feature.handler.post { remove(this) }
    }
  }

  private val msm by lazy { context.getSystemService(MediaSessionManager::class.java) }
  private val players = HashMap<android.media.session.MediaSession.Token, Player>()
  private var nextId = 1
  private var listening = false

  private val sessionsListener = MediaSessionManager.OnActiveSessionsChangedListener { list ->
    update(list.orEmpty())
  }

  /** The notification listener is enabled: we may now watch other apps' sessions. */
  fun onListenerConnected() {
    feature.handler.post {
      if (listening) return@post
      try {
        val cn = feature.notifications.component
        msm.addOnActiveSessionsChangedListener(sessionsListener, cn, feature.handler)
        listening = true
        update(msm.getActiveSessions(cn))
        Log.i(TAG, "watching media sessions")
      } catch (e: Exception) {
        Log.w(TAG, "media sessions: $e")
      }
    }
  }

  fun onConnected() {
    for (p in players.values) {
      p.lastKey = null
      send(p)
    }
  }

  fun onDisconnected() {
    for (p in players.values) p.lastKey = null
  }

  private fun update(list: List<MediaController>) {
    val tokens = list.map { it.sessionToken }.toSet()
    for (p in players.values.filter { it.controller.sessionToken !in tokens }) remove(p)
    for (c in list) {
      if (c.sessionToken in players || c.packageName == context.packageName) continue
      val p = Player(nextId++, c)
      players[c.sessionToken] = p
      c.registerCallback(p, feature.handler)
      Log.i(TAG, "session ${p.id}: ${c.packageName}")
      send(p)
    }
  }

  private fun remove(p: Player) {
    if (players.remove(p.controller.sessionToken) == null) return
    p.controller.unregisterCallback(p)
    feature.connection.send(Wire.MEDIA_CLOSE) { u32(p.id) }
    Log.i(TAG, "session ${p.id} gone")
  }

  private fun send(p: Player) {
    if (!feature.connection.connected) return
    val c = p.controller
    val md = c.metadata
    val ps = c.playbackState
    val state =
      when (ps?.state) {
        PlaybackState.STATE_PLAYING,
        PlaybackState.STATE_BUFFERING,
        PlaybackState.STATE_FAST_FORWARDING,
        PlaybackState.STATE_REWINDING -> 2
        PlaybackState.STATE_PAUSED,
        PlaybackState.STATE_CONNECTING -> 1
        else -> 0
      }
    val a = ps?.actions ?: 0L
    var actions = 0
    if (a and (PlaybackState.ACTION_PLAY or PlaybackState.ACTION_PLAY_PAUSE) != 0L)
      actions = actions or 1
    if (a and (PlaybackState.ACTION_PAUSE or PlaybackState.ACTION_PLAY_PAUSE) != 0L)
      actions = actions or 2
    if (a and PlaybackState.ACTION_SKIP_TO_NEXT != 0L) actions = actions or 4
    if (a and PlaybackState.ACTION_SKIP_TO_PREVIOUS != 0L) actions = actions or 8
    if (a and PlaybackState.ACTION_SEEK_TO != 0L) actions = actions or 16
    if (a and PlaybackState.ACTION_STOP != 0L) actions = actions or 32
    val duration =
      md?.getLong(MediaMetadata.METADATA_KEY_DURATION)?.takeIf { it > 0 }?.times(1000) ?: -1L
    val speed = ps?.playbackSpeed ?: 1f
    val position =
      if (ps == null || ps.position < 0) -1L
      else {
        var pos = ps.position
        if (state == 2 && ps.lastPositionUpdateTime > 0) {
          pos += ((SystemClock.elapsedRealtime() - ps.lastPositionUpdateTime) * speed).toLong()
        }
        pos.coerceAtLeast(0) * 1000
      }
    val title =
      md?.getString(MediaMetadata.METADATA_KEY_TITLE)
        ?: md?.getString(MediaMetadata.METADATA_KEY_DISPLAY_TITLE)
        ?: ""
    val artist =
      md?.getString(MediaMetadata.METADATA_KEY_ARTIST)
        ?: md?.getString(MediaMetadata.METADATA_KEY_ALBUM_ARTIST)
        ?: md?.getString(MediaMetadata.METADATA_KEY_DISPLAY_SUBTITLE)
        ?: ""
    val album = md?.getString(MediaMetadata.METADATA_KEY_ALBUM) ?: ""
    val art: Bitmap? =
      md?.getBitmap(MediaMetadata.METADATA_KEY_ART)
        ?: md?.getBitmap(MediaMetadata.METADATA_KEY_ALBUM_ART)
        ?: md?.getBitmap(MediaMetadata.METADATA_KEY_DISPLAY_ICON)
    // Position-only changes don't need a message: the bridge extrapolates while playing. A
    // position that jumps (a seek) does.
    val key =
      listOf(title, artist, album, state, actions, duration, speed, art?.generationId).toString()
    val expected = p.expectedPosition()
    val seeked = position >= 0 && expected >= 0 && kotlin.math.abs(position - expected) > 1_500_000
    if (key == p.lastKey && !seeked) return
    p.lastKey = key
    p.sentPosition = position
    p.sentAt = SystemClock.elapsedRealtime()
    p.sentPlaying = state == 2
    p.sentSpeed = speed
    val w = Wire.Writer(4096)
    w.u32(1)
      .text(c.packageName, 255)
      .text(label(c.packageName), 256)
      .text(title, 1024)
      .text(artist, 1024)
      .text(album, 1024)
      .u8(state)
      .u8(0)
      .u16(0)
      .u32(actions)
      .u64(duration)
      .u64(position)
      .f32(if (speed.isFinite() && kotlin.math.abs(speed) <= 16f) speed else 1f)
      .image(art?.toRgba(512))
    val blob = w.bytes()
    feature.connection.send(
      Wire.MEDIA,
      blob,
      onReply = { if (!it.ok) Log.w(TAG, "media ${p.id}: ${it.status}") },
    ) {
      u32(p.id).u32(blob.size)
    }
  }

  private fun label(pkg: String): String =
    try {
      val pm = context.packageManager
      pm.getApplicationInfo(pkg, 0).loadLabel(pm).toString()
    } catch (_: Exception) {
      pkg
    }

  fun onCommand(player: Int, cmd: Int, arg: Long) {
    val p = players.values.firstOrNull { it.id == player } ?: return
    val t = p.controller.transportControls
    Log.i(TAG, "desktop: session $player command $cmd")
    when (cmd) {
      PLAY -> t.play()
      PAUSE -> t.pause()
      PLAY_PAUSE ->
        when (p.controller.playbackState?.state) {
          PlaybackState.STATE_PLAYING,
          PlaybackState.STATE_BUFFERING -> t.pause()
          else -> t.play()
        }
      STOP -> t.stop()
      NEXT -> t.skipToNext()
      PREVIOUS -> t.skipToPrevious()
      SEEK -> {
        val ps = p.controller.playbackState ?: return
        var pos = ps.position
        if (ps.state == PlaybackState.STATE_PLAYING && ps.lastPositionUpdateTime > 0) {
          pos +=
            ((SystemClock.elapsedRealtime() - ps.lastPositionUpdateTime) * ps.playbackSpeed)
              .toLong()
        }
        t.seekTo((pos + arg / 1000).coerceAtLeast(0))
      }
      SET_POSITION -> t.seekTo((arg / 1000).coerceAtLeast(0))
      RAISE -> raise(p)
    }
  }

  private fun raise(p: Player) {
    val wm = (context.applicationContext as? InnkeeperSystemApp)?.windows
    try {
      if (wm != null) wm.launch(WindowManagerFeature.LaunchRequest(p.controller.packageName))
      else p.controller.sessionActivity?.send()
    } catch (e: Exception) {
      Log.w(TAG, "raise ${p.controller.packageName}: $e")
    }
  }

  companion object {
    const val TAG = "InnkeeperMedia"
    // MEDIA_CMD commands.
    const val PLAY = 1
    const val PAUSE = 2
    const val PLAY_PAUSE = 3
    const val STOP = 4
    const val NEXT = 5
    const val PREVIOUS = 6
    const val SEEK = 7
    const val SET_POSITION = 8
    const val RAISE = 9
  }
}
