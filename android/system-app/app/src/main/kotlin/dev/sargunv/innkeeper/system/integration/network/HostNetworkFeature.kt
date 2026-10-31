// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.integration.network

import android.content.Context
import android.net.LinkProperties
import android.net.NetworkAgent
import android.net.NetworkAgentConfig
import android.net.NetworkCapabilities
import android.net.NetworkScore
import android.os.FileObserver
import android.os.Handler
import android.os.HandlerThread
import android.util.Log
import dev.sargunv.innkeeper.system.Feature
import java.io.File
import java.net.InetAddress

/**
 * Android shares the host's network namespace, as any desktop app does. Offer the host's network as
 * Android's default network: an agent with no interface, whose DNS servers are the host's. Sockets
 * use the host's routes.
 *
 * The resolvers come from the sandbox's `/etc/resolv.conf`, which Flatpak keeps current (a link
 * into `/run/host/monitor`), and are sent again whenever it changes, such as when the host joins a
 * VPN or another Wi-Fi network. The host connection's metering ([setMetered], from the host state
 * service's NetworkMonitor portal reading) is the agent's `NOT_METERED`, so apps that honour
 * "unmetered only" (F-Droid, AntennaPod) see the host's state.
 */
class HostNetworkFeature(private val context: Context) : Feature {
  override val name = "host network"

  private var thread: HandlerThread? = null
  @Volatile private var agent: NetworkAgent? = null
  @Volatile private var metered = false
  private var watch: FileObserver? = null
  private var dns: List<InetAddress> = emptyList()

  override fun start() {
    val t = HandlerThread("HostNetwork").also { it.start() }
    thread = t
    dns = resolvers()
    val a =
      object :
        NetworkAgent(
          context,
          t.looper,
          TAG,
          capabilities(),
          linkProperties(dns),
          NetworkScore.Builder().build(),
          NetworkAgentConfig.Builder().build(),
          null,
        ) {}
    a.register()
    a.markConnected()
    agent = a
    Log.i(TAG, "host network up, DNS $dns")
    watch(Handler(t.looper))
  }

  /** Marks the host network metered or not, live. */
  fun setMetered(m: Boolean) {
    val t = thread ?: return
    Handler(t.looper).post {
      val a = agent ?: return@post
      if (m == metered) return@post
      metered = m
      a.sendNetworkCapabilities(capabilities())
      Log.i(TAG, "host network ${if (m) "metered" else "not metered"}")
    }
  }

  /**
   * Watches the directory of the file `/etc/resolv.conf` resolves to (Flatpak replaces it there),
   * and sends the agent the new resolvers when they change.
   */
  private fun watch(handler: Handler) {
    val dir = runCatching { File(RESOLV_CONF).canonicalFile.parentFile }.getOrNull() ?: return
    val mask =
      FileObserver.CLOSE_WRITE or
        FileObserver.MOVED_TO or
        FileObserver.CREATE or
        FileObserver.DELETE
    val w =
      object : FileObserver(dir, mask) {
        override fun onEvent(event: Int, path: String?) {
          handler.post { refresh() }
        }
      }
    w.startWatching()
    watch = w
  }

  private fun refresh() {
    val a = agent ?: return
    val now = resolvers()
    if (now.isEmpty() || now == dns) return
    dns = now
    a.sendLinkProperties(linkProperties(now))
    Log.i(TAG, "host DNS changed: $now")
  }

  /** `nameserver` lines of the sandbox's resolv.conf, as numeric addresses (no lookup). */
  private fun resolvers(): List<InetAddress> = runCatching {
    File(RESOLV_CONF).readLines()
  }
    .getOrDefault(emptyList())
    .mapNotNull { l ->
      val w = l.trim().split(Regex("\\s+"))
      if (w.size >= 2 && w[0] == "nameserver") runCatching { numeric(w[1]) }.getOrNull() else null
    }

  private fun numeric(a: String): InetAddress {
    // A scoped IPv6 address (fe80::1%wlan0) names a host interface Android doesn't have.
    val addr = a.substringBefore('%')
    require(addr.isNotEmpty() && addr.all { it.isDigit() || it in "abcdefABCDEF.:" })
    return InetAddress.getByName(addr)
  }

  private fun linkProperties(dns: List<InetAddress>): LinkProperties {
    val props = LinkProperties()
    LinkProperties::class.java.getMethod("setDnsServers", Collection::class.java).invoke(props, dns)
    return props
  }

  /** What a wired default network offers (NetworkCapabilities.Builder is a system API). */
  private fun capabilities(): NetworkCapabilities {
    val b = Class.forName("android.net.NetworkCapabilities\$Builder").getConstructor().newInstance()
    val c = b.javaClass
    c.getMethod("addTransportType", Int::class.java)
      .invoke(b, NetworkCapabilities.TRANSPORT_ETHERNET)
    for (cap in
      listOf(
        NetworkCapabilities.NET_CAPABILITY_INTERNET,
        NetworkCapabilities.NET_CAPABILITY_NOT_RESTRICTED,
        NetworkCapabilities.NET_CAPABILITY_TRUSTED,
        NetworkCapabilities.NET_CAPABILITY_NOT_VPN,
        NetworkCapabilities.NET_CAPABILITY_NOT_ROAMING,
        NetworkCapabilities.NET_CAPABILITY_NOT_CONGESTED,
        NetworkCapabilities.NET_CAPABILITY_NOT_SUSPENDED,
        NET_CAPABILITY_NOT_VCN_MANAGED,
      )) c.getMethod("addCapability", Int::class.java).invoke(b, cap)
    if (!metered) {
      c.getMethod("addCapability", Int::class.java)
        .invoke(b, NetworkCapabilities.NET_CAPABILITY_NOT_METERED)
    }
    c.getMethod("setLinkUpstreamBandwidthKbps", Int::class.java).invoke(b, LINK_KBPS)
    c.getMethod("setLinkDownstreamBandwidthKbps", Int::class.java).invoke(b, LINK_KBPS)
    return c.getMethod("build").invoke(b) as NetworkCapabilities
  }

  companion object {
    private const val TAG = "InnkeeperHostNetwork"
    private const val RESOLV_CONF = "/etc/resolv.conf"
    private const val LINK_KBPS = 1_000_000
    /** NetworkCapabilities.NET_CAPABILITY_NOT_VCN_MANAGED (a system API constant). */
    private const val NET_CAPABILITY_NOT_VCN_MANAGED = 28
  }
}
