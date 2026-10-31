// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.apps

import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.net.Uri

/** Exported VIEW filters that can receive the content URIs supplied by the desktop. */
object FileAssociations {
  fun mimeTypes(pm: PackageManager, pkg: String): List<String> {
    // @SystemApi: queryIntentActivities deduplicates activities, returning only one filter
    // per activity. Enumerate candidates first, then resolve each type to check visibility.
    @Suppress("UNCHECKED_CAST")
    val filters =
      PackageManager::class
        .java
        .getMethod("getAllIntentFilters", String::class.java)
        .invoke(pm, pkg) as List<IntentFilter>
    val types =
      fileMimeTypes(
        filters
          .filter {
            it.hasAction(Intent.ACTION_VIEW) && it.hasCategory(Intent.CATEGORY_DEFAULT)
          }
          .flatMap { it.typesIterator()?.asSequence()?.toList().orEmpty() },
      )
    return types.filter { type ->
      pm
        .queryIntentActivities(
          Intent(Intent.ACTION_VIEW)
            .setPackage(pkg)
            .setDataAndType(
              Uri.parse("content://dev.sargunv.innkeeper.system.documents/document/probe"),
              type,
            ),
          PackageManager.ResolveInfoFlags.of(
            (PackageManager.GET_RESOLVED_FILTER or PackageManager.MATCH_DEFAULT_ONLY).toLong(),
          ),
        )
        .any {
          it.activityInfo.exported &&
            it.activityInfo.enabled &&
            type in fileMimeTypes(it.filter?.typesIterator()?.asSequence()?.toList().orEmpty())
        }
    }
  }
}
