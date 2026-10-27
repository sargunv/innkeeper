// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system

/** android.os.SystemProperties (hidden API; platform-signed apps may use it). */
object SystemProps {
  private val get by lazy {
    Class.forName("android.os.SystemProperties")
      .getMethod("get", String::class.java, String::class.java)
  }

  fun get(key: String, default: String = ""): String =
    try {
      get.invoke(null, key, default) as String
    } catch (_: ReflectiveOperationException) {
      default
    }

  fun getBoolean(key: String, default: Boolean): Boolean =
    when (get(key).lowercase()) {
      "1",
      "true",
      "y",
      "yes",
      "on" -> true
      "0",
      "false",
      "n",
      "no",
      "off" -> false
      else -> default
    }
}
