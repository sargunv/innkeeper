// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.apps

import android.app.IActivityManager
import android.app.usage.StorageStatsManager
import android.content.Context
import android.content.pm.ApplicationInfo
import android.content.pm.IPackageDataObserver
import android.content.pm.IPackageManager
import android.content.pm.PackageManager
import android.content.pm.PermissionInfo
import android.os.Process
import android.os.ServiceManager
import android.os.storage.StorageManager
import android.permission.IPermissionManager
import dev.sargunv.innkeeper.system.input.PackageInputSettings
import dev.sargunv.innkeeper.system.integration.IntegrationFeature
import java.util.concurrent.CompletableFuture
import java.util.concurrent.TimeUnit
import org.json.JSONArray
import org.json.JSONObject

/** Android operations requested by the desktop session API, on the apps worker. */
class AppSettings(private val context: Context) {
  private val pm = context.packageManager
  private val user = Process.myUserHandle()
  private val userId = Process.myUid() / PER_USER_RANGE
  private val packages by lazy {
    IPackageManager.Stub.asInterface(ServiceManager.getService("package"))
  }
  private val activities by lazy {
    IActivityManager.Stub.asInterface(ServiceManager.getService("activity"))
  }
  private val permissionService by lazy {
    IPermissionManager.Stub.asInterface(ServiceManager.getService("permissionmgr"))
  }
  private val prefs =
    context
      .createDeviceProtectedStorageContext()
      .getSharedPreferences("app-input", Context.MODE_PRIVATE)
  val input =
    PackageInputSettings(
      { prefs.getBoolean(it, false) },
      { pkg, enabled ->
        check(prefs.edit().putBoolean(pkg, enabled).commit()) { "Could not save input setting" }
      },
    )

  fun execute(command: JSONObject): JSONObject {
    return try {
      val pkg = command.getString("package")
      val ai = pm.getApplicationInfo(pkg, 0)
      require(
        ai.flags and (ApplicationInfo.FLAG_SYSTEM or ApplicationInfo.FLAG_UPDATED_SYSTEM_APP) == 0,
      ) {
        "Only installed user apps can be managed"
      }
      val enabled = command.getBoolean("enabled")
      when (command.getString("action")) {
        "info" -> {}
        "escape-back" -> input.setEscapeBack(pkg, enabled)
        "background" -> {
          val lifecycle =
            checkNotNull(IntegrationFeature.instance?.lifecycle) { "Lifecycle is unavailable" }
          lifecycle.background.set(pkg, enabled)?.let { error(it) }
          lifecycle.poke()
        }
        "force-stop" -> forceStop(pkg)
        "clear-cache" -> observe { observer -> packages.deleteApplicationCacheFiles(pkg, observer) }
        "clear-data" ->
          observe { observer ->
            check(activities.clearApplicationUserData(pkg, false, observer, userId)) {
              "Clear data was refused"
            }
          }
        "permission" -> {
          val permission = command.getString("permission")
          val row = permissions(pkg).firstOrNull { it.getString("name") == permission }
          require(row != null && row.getBoolean("editable")) {
            "Permission is not a changeable runtime permission"
          }
          if (enabled) {
            permissionService.grantRuntimePermission(pkg, permission, DEFAULT_DEVICE, userId)
          } else {
            permissionService.revokeRuntimePermission(pkg, permission, DEFAULT_DEVICE, userId, null)
          }
        }
        else -> error("Unknown app action")
      }
      JSONObject().put("error", "").put("info", info(pkg))
    } catch (e: Exception) {
      val cause = e.cause ?: e
      JSONObject()
        .put("error", (cause.message ?: cause.toString()).take(1024))
        .put("info", JSONObject.NULL)
    }
  }

  private fun forceStop(pkg: String) {
    activities.forceStopPackage(pkg, userId)
  }

  private fun observe(start: (IPackageDataObserver) -> Unit) {
    val done = CompletableFuture<Boolean>()
    start(
      object : IPackageDataObserver.Stub() {
        override fun onRemoveCompleted(packageName: String?, succeeded: Boolean) {
          done.complete(succeeded)
        }
      },
    )
    check(done.get(12, TimeUnit.SECONDS)) { "Android could not clear the app's storage" }
  }

  private fun permissions(pkg: String): List<JSONObject> {
    val pi = pm.getPackageInfo(pkg, PackageManager.GET_PERMISSIONS)
    return pi.requestedPermissions.orEmpty().mapNotNull { permission ->
      val definition =
        try {
          pm.getPermissionInfo(permission, 0)
        } catch (_: PackageManager.NameNotFoundException) {
          return@mapNotNull null
        }
      if (definition.protection != PermissionInfo.PROTECTION_DANGEROUS) return@mapNotNull null
      val flags = permissionService.getPermissionFlags(pkg, permission, DEFAULT_DEVICE, userId)
      val fixed = ((1 shl 4) or (1 shl 2)) // PackageManager: SYSTEM_FIXED | POLICY_FIXED (hidden)
      JSONObject()
        .put("name", permission)
        .put("granted", pm.checkPermission(permission, pkg) == PackageManager.PERMISSION_GRANTED)
        .put("editable", pi.applicationInfo!!.targetSdkVersion >= 23 && flags and fixed == 0)
    }
  }

  private fun info(pkg: String): JSONObject {
    val storage =
      context
        .getSystemService(StorageStatsManager::class.java)
        .queryStatsForPackage(StorageManager.UUID_DEFAULT, pkg, user)
    return JSONObject()
      .put("app_bytes", storage.appBytes)
      .put("data_bytes", storage.dataBytes)
      .put("cache_bytes", storage.cacheBytes)
      .put("escape_back", input.escapeBack(pkg))
      .put(
        "background",
        IntegrationFeature.instance?.lifecycle?.background?.allowed()?.contains(pkg) == true,
      )
      .put("permissions", JSONArray(permissions(pkg)))
  }

  private companion object {
    /** UserHandle.PER_USER_RANGE (hidden). */
    const val PER_USER_RANGE = 100_000
    /** VirtualDeviceManager.PERSISTENT_DEVICE_ID_DEFAULT (hidden), which is the device itself. */
    const val DEFAULT_DEVICE = "default:0"
  }
}
