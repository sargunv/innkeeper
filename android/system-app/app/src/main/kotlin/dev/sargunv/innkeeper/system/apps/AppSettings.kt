// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.apps

import android.app.ActivityManager
import android.app.usage.StorageStatsManager
import android.content.Context
import android.content.pm.ApplicationInfo
import android.content.pm.IPackageDataObserver
import android.content.pm.PackageManager
import android.content.pm.PermissionInfo
import android.os.Process
import android.os.storage.StorageManager
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
        "clear-cache" ->
          observe { observer ->
            pm.javaClass
              .getMethod(
                "deleteApplicationCacheFiles",
                String::class.java,
                IPackageDataObserver::class.java,
              )
              .invoke(pm, pkg, observer)
          }
        "clear-data" ->
          observe { observer ->
            val am = context.getSystemService(ActivityManager::class.java)
            check(
              am.javaClass
                .getMethod(
                  "clearApplicationUserData",
                  String::class.java,
                  IPackageDataObserver::class.java,
                )
                .invoke(am, pkg, observer) == true,
            ) {
              "Clear data was refused"
            }
          }
        "permission" -> {
          val permission = command.getString("permission")
          val row = permissions(pkg).firstOrNull { it.getString("name") == permission }
          require(row != null && row.getBoolean("editable")) {
            "Permission is not a changeable runtime permission"
          }
          pm.javaClass
            .getMethod(
              if (enabled) "grantRuntimePermission" else "revokeRuntimePermission",
              String::class.java,
              String::class.java,
              android.os.UserHandle::class.java,
            )
            .invoke(pm, pkg, permission, user)
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
    val am = context.getSystemService(ActivityManager::class.java)
    am.javaClass.getMethod("forceStopPackage", String::class.java).invoke(am, pkg)
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
      val flags =
        pm.javaClass
          .getMethod(
            "getPermissionFlags",
            String::class.java,
            String::class.java,
            android.os.UserHandle::class.java,
          )
          .invoke(pm, permission, pkg, user) as Int
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
}
