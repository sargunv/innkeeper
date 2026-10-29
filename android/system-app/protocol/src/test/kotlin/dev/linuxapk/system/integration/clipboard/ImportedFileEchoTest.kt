// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.system.integration.clipboard

import kotlin.test.Test
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class ImportedFileEchoTest {
  @Test
  fun retainedImportedUriCanBeCopiedAgainAfterText() {
    val echo = ImportedFileEcho()
    val uri =
      listOf(
        "content://dev.linuxapk.system.clipboard.files/retained/notes.txt",
        "content://dev.linuxapk.system.clipboard.files/second/notes.txt",
      )
    val generation = echo.install(uri)
    assertTrue(echo.consume(uri, generation)) // Installing the host clip must not echo it.
    assertFalse(echo.consume(emptyList(), 0)) // Android copies text.
    assertFalse(echo.consume(uri, 0)) // A new ClipData reuses the retained URI.
    assertFalse(echo.consume(uri, generation)) // Even retaining the extras cannot suppress it.

    val partial = echo.install(uri)
    assertFalse(
      echo.consume(uri.take(1), partial),
    ) // Matching the first item cannot hide a changed selection.
    val newer = echo.install(uri)
    assertFalse(echo.consume(uri, generation)) // An old generation cannot match a new install.
    assertFalse(echo.consume(uri, newer)) // Every observation consumes the pending marker.
  }
}
