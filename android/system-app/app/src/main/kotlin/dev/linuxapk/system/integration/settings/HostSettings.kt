// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.integration.settings

import android.app.AlarmManager
import android.app.UiModeManager
import android.content.Context
import android.content.Intent
import android.icu.util.LocaleData
import android.icu.util.ULocale
import android.os.LocaleList
import android.provider.Settings
import android.util.Log
import dev.linuxapk.system.bridge.Wire
import java.nio.ByteBuffer
import java.util.Locale
import java.util.TimeZone
import org.json.JSONObject

/** One `SETTINGS` event (host/bridge/src/system/proto.rs); 0 or empty means unknown. */
data class DesktopSettings(
  val colorScheme: Int,
  val accent: Int?,
  val followAccent: Boolean,
  val contrast: Int,
  val reducedMotion: Int,
  val clock: Int,
  val textScale: Float,
  val timezone: String,
  val locales: String,
  val timeLocale: String,
  val measureLocale: String,
) {
  companion object {
    fun decode(r: ByteBuffer): DesktopSettings {
      val scheme = r.get().toInt() and 0xff
      val hasAccent = r.get().toInt() and 0xff
      val rgb =
        ((r.get().toInt() and 0xff) shl 16) or
          ((r.get().toInt() and 0xff) shl 8) or
          (r.get().toInt() and 0xff)
      return DesktopSettings(
        colorScheme = scheme,
        accent = if (hasAccent == 1) rgb else null,
        followAccent = hasAccent != 2,
        contrast = r.get().toInt() and 0xff,
        reducedMotion = r.get().toInt() and 0xff,
        clock = r.get().toInt() and 0xff,
        textScale = r.float,
        timezone = Wire.name(r),
        locales = Wire.name(r),
        timeLocale = Wire.name(r),
        measureLocale = Wire.name(r),
      )
    }
  }
}

/**
 * Applies the desktop's settings to Android, live: dark mode (UiModeManager), the accent color as
 * the Material You seed (SystemUI's theme customization setting), contrast, reduced motion, the
 * time zone, the locales with their regional preferences, the 12/24-hour clock and the font scale.
 * Each setting is applied when the desktop's value changes (and once after connecting, if it
 * differs from Android's), so a change made inside Android lasts until the desktop changes that
 * setting again.
 */
class HostSettings(private val context: Context) {
  private var last: DesktopSettings? = null
  private val cr = context.contentResolver

  fun apply(s: DesktopSettings) {
    val prev = last
    last = s
    Log.i(TAG, "desktop settings: $s")
    fun changed(f: (DesktopSettings) -> Any?) = prev == null || f(prev) != f(s)
    step("dark mode") { if (changed { it.colorScheme }) darkMode(s.colorScheme) }
    step("accent") {
      if (s.followAccent && (changed { it.accent } || changed { it.followAccent }))
        accent(s.accent, prev == null)
    }
    step("contrast") { if (changed { it.contrast }) contrast(s.contrast, prev == null) }
    step("motion") { if (changed { it.reducedMotion }) motion(s.reducedMotion, prev == null) }
    step("time zone") { if (changed { it.timezone }) timezone(s.timezone) }
    step("locales") {
      if (changed { Triple(it.locales, it.timeLocale, it.measureLocale) }) {
        locales(s.locales, s.timeLocale, s.measureLocale)
      }
    }
    step("clock") { if (changed { it.clock }) clock(s.clock) }
    step("font scale") { if (changed { it.textScale }) fontScale(s.textScale) }
  }

  /** Forgets what was applied, so the next event applies everything (after a reconnect). */
  fun reset() {
    last = null
  }

  private inline fun step(what: String, f: () -> Unit) {
    try {
      f()
    } catch (e: Exception) {
      Log.w(TAG, "$what: $e")
    }
  }

  private fun darkMode(scheme: Int) {
    val mode =
      when (scheme) {
        1 -> UiModeManager.MODE_NIGHT_YES
        2 -> UiModeManager.MODE_NIGHT_NO
        else -> return // no preference: leave Android's choice
      }
    val ui = context.getSystemService(UiModeManager::class.java)
    if (ui.nightMode != mode) {
      ui.setNightMode(mode)
      Log.i(TAG, "night mode -> ${if (mode == UiModeManager.MODE_NIGHT_YES) "yes" else "no"}")
    }
  }

  /**
   * The accent becomes the Material You seed. Android 17's `IThemeManager` (service "theme") is
   * behind the `enable_theme_service` flag, off in current images; SystemUI's
   * ThemeOverlayController applies the same "preset" seed from the theme customization setting.
   */
  private fun accent(rgb: Int?, first: Boolean) {
    val json =
      try {
        JSONObject(Settings.Secure.getString(cr, THEME_KEY) ?: "{}")
      } catch (_: Exception) {
        JSONObject()
      }
    if (rgb == null) {
      // The desktop has no accent: go back to wallpaper colors, if we had set one.
      if (first || json.optString(OURS) != "1") return
      json.remove(PALETTE)
      json.remove(ACCENT)
      json.remove(OURS)
      json.put(SOURCE, "home_wallpaper")
    } else {
      val hex = String.format("%06X", rgb and 0xffffff)
      if (json.optString(PALETTE) == hex && json.optString(SOURCE) == "preset") return
      json.put(PALETTE, hex)
      json.put(ACCENT, hex)
      json.put(SOURCE, "preset")
      json.put(OURS, "1")
      if (!json.has(STYLE)) json.put(STYLE, "TONAL_SPOT")
    }
    json.put("_applied_timestamp", System.currentTimeMillis())
    Settings.Secure.putString(cr, THEME_KEY, json.toString())
    Log.i(TAG, "theme customization -> $json")
  }

  private fun contrast(c: Int, first: Boolean) {
    if (first && c == 0) return // don't reset a contrast chosen in Android at every boot
    val level = if (c == 1) 1f else 0f
    if (Settings.Secure.getFloat(cr, "contrast_level", 0f) != level) {
      Settings.Secure.putFloat(cr, "contrast_level", level)
      Log.i(TAG, "contrast -> $level")
    }
  }

  private fun motion(reduced: Int, first: Boolean) {
    if (first && reduced == 0) return
    val scale = if (reduced == 1) 0f else 1f
    for (key in
      listOf(
        Settings.Global.ANIMATOR_DURATION_SCALE,
        Settings.Global.TRANSITION_ANIMATION_SCALE,
        Settings.Global.WINDOW_ANIMATION_SCALE,
      )) {
      Settings.Global.putFloat(cr, key, scale)
    }
    Log.i(TAG, "animation scales -> $scale")
  }

  private fun timezone(tz: String) {
    if (tz.isEmpty() || tz == TimeZone.getDefault().id) return
    if (!TimeZone.getAvailableIDs().contains(tz)) {
      Log.w(TAG, "unknown time zone $tz")
      return
    }
    // Automatic detection would reject a manual zone (and telephony or location could fight it).
    Settings.Global.putInt(cr, Settings.Global.AUTO_TIME_ZONE, 0)
    context.getSystemService(AlarmManager::class.java).setTimeZone(tz)
    Log.i(TAG, "time zone -> $tz")
  }

  /**
   * The desktop's languages, with its regional formats as Android's regional preferences: the
   * Unicode extensions Settings writes (`fw` first day of week from `LC_TIME`, `mu` temperature
   * unit from `LC_MEASUREMENT`), on every locale and in `locale_preferences`, when they differ from
   * the language's own defaults.
   */
  private fun locales(tags: String, time: String, measure: String) {
    if (tags.isEmpty()) return
    val base = LocaleList.forLanguageTags(tags)
    if (base.isEmpty) return
    val prefs = regional(base[0], time, measure)
    val list =
      if (prefs.isEmpty()) base
      else
        LocaleList(
          *Array(base.size()) { i ->
            val b = Locale.Builder().setLocale(base[i])
            for ((k, v) in prefs) b.setUnicodeLocaleKeyword(k, v)
            b.build()
          },
        )
    val und = Locale.Builder().setLanguage("und")
    for ((k, v) in prefs) und.setUnicodeLocaleKeyword(k, v)
    val prefTag = if (prefs.isEmpty()) "" else und.build().toLanguageTag()
    try {
      if (Settings.System.getString(cr, LOCALE_PREFERENCES).orEmpty() != prefTag) {
        Settings.System.putString(cr, LOCALE_PREFERENCES, prefTag)
      }
    } catch (e: Exception) {
      Log.w(TAG, "locale preferences: $e")
    }
    if (list.toLanguageTags() == LocaleList.getDefault().toLanguageTags()) return
    // com.android.internal.app.LocalePicker.updateLocales (CHANGE_CONFIGURATION).
    Class.forName("com.android.internal.app.LocalePicker")
      .getMethod("updateLocales", LocaleList::class.java)
      .invoke(null, list)
    Log.i(TAG, "locales -> ${list.toLanguageTags()}")
  }

  /** Unicode keywords for [time]'s week start and [measure]'s unit where [lang] differs. */
  private fun regional(lang: Locale, time: String, measure: String): Map<String, String> {
    val out = linkedMapOf<String, String>()
    val l = ULocale.forLocale(lang)
    if (time.isNotEmpty()) {
      val fw = firstDay(ULocale.forLanguageTag(time))
      if (fw != firstDay(l)) out["fw"] = DAYS[fw - 1]
    }
    if (measure.isNotEmpty()) {
      val mu = temperature(ULocale.forLanguageTag(measure))
      if (mu != temperature(l)) out["mu"] = mu
    }
    return out
  }

  private fun firstDay(l: ULocale): Int = android.icu.util.Calendar.getInstance(l).firstDayOfWeek

  private fun temperature(l: ULocale): String =
    if (
      LocaleData.getMeasurementSystem(ULocale.addLikelySubtags(l)) ==
        LocaleData.MeasurementSystem.US
    ) {
      "fahrenhe"
    } else {
      "celsius"
    }

  private fun clock(c: Int) {
    val value =
      when (c) {
        1 -> "12"
        2 -> "24"
        else -> return
      }
    if (Settings.System.getString(cr, Settings.System.TIME_12_24) == value) return
    Settings.System.putString(cr, Settings.System.TIME_12_24, value)
    // As Settings does: clocks listen for TIME_CHANGED with the preference (a protected broadcast,
    // which persistent apps may send).
    val i =
      Intent(Intent.ACTION_TIME_CHANGED)
        .addFlags(FLAG_RECEIVER_INCLUDE_BACKGROUND)
        .putExtra("android.intent.extra.TIME_PREF_24_HOUR_FORMAT", if (c == 2) 1 else 0)
    context.sendBroadcast(i)
    Log.i(TAG, "clock -> $value-hour")
  }

  private fun fontScale(scale: Float) {
    if (scale <= 0f) return
    val v = scale.coerceIn(0.85f, 2.0f)
    if (Settings.System.getFloat(cr, Settings.System.FONT_SCALE, 1f) != v) {
      Settings.System.putFloat(cr, Settings.System.FONT_SCALE, v)
      Log.i(TAG, "font scale -> $v")
    }
  }

  companion object {
    const val TAG = "LinuxApkSettings"
    private const val THEME_KEY = "theme_customization_overlay_packages"
    /** Settings.System.LOCALE_PREFERENCES (hidden): what Settings shows as regional preferences. */
    private const val LOCALE_PREFERENCES = "locale_preferences"
    private val DAYS = arrayOf("sun", "mon", "tue", "wed", "thu", "fri", "sat")
    private const val PALETTE = "android.theme.customization.system_palette"
    private const val ACCENT = "android.theme.customization.accent_color"
    private const val SOURCE = "android.theme.customization.color_source"
    private const val STYLE = "android.theme.customization.theme_style"
    /** Marks a seed we set (so "no accent" only undoes our own). */
    private const val OURS = "dev.linuxapk.from_desktop"
    /** Intent.FLAG_RECEIVER_INCLUDE_BACKGROUND (hidden). */
    private const val FLAG_RECEIVER_INCLUDE_BACKGROUND = 0x01000000
  }
}
