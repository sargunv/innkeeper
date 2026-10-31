// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.apps

import kotlin.test.Test
import kotlin.test.assertEquals

class FileMimeTypesTest {
  @Test
  fun shareBatchUsesTheNarrowestCommonType() {
    assertEquals("image/png", sharedMimeType(listOf("image/png", "image/png")))
    assertEquals("image/*", sharedMimeType(listOf("image/png", "image/jpeg")))
    assertEquals("*/*", sharedMimeType(listOf("image/png", "text/plain")))
  }

  @Test
  fun concreteTypesExcludeWildcardsSchemesAndInvalidValues() {
    assertEquals(
      listOf("application/vnd.example+json", "text/markdown"),
      fileMimeTypes(
        listOf(
          "text/markdown",
          "*/*",
          "*",
          "text",
          "text/*",
          "http",
          "https",
          "file",
          "content",
          "intent",
          "text/markdown",
          "text/plain;Name=x",
          "text/plain\nName=x",
          "application/vnd.example+json",
        ),
      ),
    )
  }
}
