// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.net

import android.content.Context
import android.net.LinkProperties
import android.net.NetworkAgent
import android.net.NetworkAgentConfig
import android.net.NetworkCapabilities
import android.net.NetworkScore
import android.os.HandlerThread
import android.util.Log
import dev.linuxapk.system.Feature
import dev.linuxapk.system.SystemProps
import java.net.InetAddress

/**
 * Flatpak mode (docs/session.md): Android shares the host's network namespace, as any desktop app
 * does, so there is no interface for EthernetService to bring up. Offer the host's network itself
 * as Android's default network: an agent with no interface, whose DNS servers are the host's
 * (`ro.vendor.linuxapk.host_net.dns`, set by the launcher). Sockets then use the host's routes.
 * Outside Flatpak mode the property is unset and this does nothing.
 */
class HostNetworkFeature(private val context: Context) : Feature {
  override val name = "host network"

  private var thread: HandlerThread? = null
  private var agent: NetworkAgent? = null

  override fun start() {
    val dns = SystemProps.get(DNS_PROP).split(',').map { it.trim() }.filter { it.isNotEmpty() }
    if (dns.isEmpty()) return
    val t = HandlerThread("HostNetwork").also { it.start() }
    thread = t
    val caps = capabilities()
    val props = LinkProperties()
    // Numeric addresses: no lookup.
    LinkProperties::class
      .java
      .getMethod("setDnsServers", Collection::class.java)
      .invoke(props, dns.map { InetAddress.getByName(it) })
    val a =
      object :
        NetworkAgent(
          context,
          t.looper,
          TAG,
          caps,
          props,
          NetworkScore.Builder().build(),
          NetworkAgentConfig.Builder().build(),
          null,
        ) {}
    a.register()
    a.markConnected()
    agent = a
    Log.i(TAG, "host network up, DNS $dns")
  }

  override fun stop() {
    agent?.unregister()
    agent = null
    thread?.quitSafely()
    thread = null
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
        NetworkCapabilities.NET_CAPABILITY_NOT_METERED,
        NetworkCapabilities.NET_CAPABILITY_NOT_RESTRICTED,
        NetworkCapabilities.NET_CAPABILITY_TRUSTED,
        NetworkCapabilities.NET_CAPABILITY_NOT_VPN,
        NetworkCapabilities.NET_CAPABILITY_NOT_ROAMING,
        NetworkCapabilities.NET_CAPABILITY_NOT_CONGESTED,
        NetworkCapabilities.NET_CAPABILITY_NOT_SUSPENDED,
        NET_CAPABILITY_NOT_VCN_MANAGED,
      )) c.getMethod("addCapability", Int::class.java).invoke(b, cap)
    c.getMethod("setLinkUpstreamBandwidthKbps", Int::class.java).invoke(b, LINK_KBPS)
    c.getMethod("setLinkDownstreamBandwidthKbps", Int::class.java).invoke(b, LINK_KBPS)
    return c.getMethod("build").invoke(b) as NetworkCapabilities
  }

  private companion object {
    const val TAG = "LinuxApkHostNetwork"
    const val DNS_PROP = "ro.vendor.linuxapk.host_net.dns"
    const val LINK_KBPS = 1_000_000
    /** NetworkCapabilities.NET_CAPABILITY_NOT_VCN_MANAGED (a system API constant). */
    const val NET_CAPABILITY_NOT_VCN_MANAGED = 28
  }
}
