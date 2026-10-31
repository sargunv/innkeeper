// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.integration.media

import android.content.Context
import android.media.AudioManager
import android.media.AudioPlaybackConfiguration
import android.util.Log
import dev.sargunv.innkeeper.system.bridge.Wire
import dev.sargunv.innkeeper.system.integration.IntegrationFeature

/**
 * Names the app on Android's mixed playback stream for the desktop mixer (GNOME Settings,
 * pavucontrol, Plasma's volume applet). Android mixes every app into one PipeWire stream, so the
 * bridge labels that stream with the app [playingApp] picks. A choice that lasts under
 * [DEBOUNCE_MS] is dropped, which keeps a track change from flashing the previous app's name. Runs
 * on the integration thread.
 */
class PlaybackApps(private val context: Context, private val feature: IntegrationFeature) {
  private val audio by lazy { context.getSystemService(AudioManager::class.java) }
  private var registered = false
  private var connected = false
  /** The package last sent, or null when the bridge was told nothing is playing. */
  private var sent: String? = null
  private var pending: String? = null
  private var pendingAt = 0L

  private val callback =
    object : AudioManager.AudioPlaybackCallback() {
      override fun onPlaybackConfigChanged(configs: List<AudioPlaybackConfiguration>) {
        feature.handler.post { onConfigs(configs) }
      }
    }

  private val flush = Runnable { flushPending(now()) }

  fun start() {
    if (registered) return
    audio.registerAudioPlaybackCallback(callback, feature.handler)
    registered = true
  }

  fun onConnected() {
    connected = true
    sent = null
    pending = null
    report(current())
  }

  fun onDisconnected() {
    connected = false
    sent = null
    pending = null
    feature.handler.removeCallbacks(flush)
  }

  private fun onConfigs(configs: List<AudioPlaybackConfiguration>) {
    val choice = playingApp(configs.mapNotNull { it.client() })
    val pkg = choice?.let { playbackPackage(it.uid, ::packagesForUid, context.packageName) }
    report(pkg)
  }

  private fun current(): String? {
    val configs = audio.activePlaybackConfigurations
    val choice = playingApp(configs.mapNotNull { it.client() })
    return choice?.let { playbackPackage(it.uid, ::packagesForUid, context.packageName) }
  }

  /** Sends [pkg] once it has held for [DEBOUNCE_MS], immediately when it matches what is held. */
  private fun report(pkg: String?) {
    val t = now()
    if (pkg == pending && pendingAt > 0 && t - pendingAt >= DEBOUNCE_MS) {
      flushPending(t)
      return
    }
    if (pkg == pending) return
    pending = pkg
    pendingAt = t
    feature.handler.removeCallbacks(flush)
    if (pkg == sent) return
    feature.handler.postDelayed(flush, DEBOUNCE_MS)
  }

  private fun flushPending(t: Long) {
    if (pending == sent || t - pendingAt < DEBOUNCE_MS) return
    send(pending)
  }

  private fun send(pkg: String?) {
    sent = pkg
    if (!connected || !feature.connection.connected) return
    val label = pkg?.let { appLabel(it) } ?: ""
    Log.i(TAG, "playback app: ${pkg ?: "(none)"}")
    feature.connection.send(Wire.PLAYBACK_APP) {
      if (pkg == null) name("") else name(pkg)
      name(label)
    }
  }

  private fun packagesForUid(uid: Int): List<String>? =
    context.packageManager.getPackagesForUid(uid)?.toList()

  private fun appLabel(pkg: String): String =
    try {
      val pm = context.packageManager
      pm.getApplicationInfo(pkg, 0).loadLabel(pm).toString()
    } catch (_: Exception) {
      pkg
    }

  private fun now(): Long = android.os.SystemClock.elapsedRealtime()

  /**
   * `getClientUid`, `getPlayerState` and `getPlayerInterfaceId` are system APIs, so the SDK jar
   * does not have them. A platform-signed app may call them; a config that has none (the public,
   * anonymized copy) is skipped.
   */
  private fun AudioPlaybackConfiguration.client(): PlaybackClient? =
    try {
      val uid = hidden("getClientUid") ?: return null
      val state = hidden("getPlayerState") ?: return null
      val started = hidden("getPlayerInterfaceId") ?: return null
      PlaybackClient(uid, started.toLong(), state == PLAYER_STATE_STARTED)
    } catch (_: ReflectiveOperationException) {
      null
    }

  private fun AudioPlaybackConfiguration.hidden(name: String): Int? =
    hiddenMethod(name).invoke(this) as? Int

  companion object {
    const val TAG = "InnkeeperPlayback"
    const val DEBOUNCE_MS = 300L

    /** AudioPlaybackConfiguration.PLAYER_STATE_STARTED. */
    private const val PLAYER_STATE_STARTED = 2

    private val methods = HashMap<String, java.lang.reflect.Method>()

    private fun hiddenMethod(name: String): java.lang.reflect.Method =
      methods.getOrPut(name) { AudioPlaybackConfiguration::class.java.getMethod(name) }
  }
}
