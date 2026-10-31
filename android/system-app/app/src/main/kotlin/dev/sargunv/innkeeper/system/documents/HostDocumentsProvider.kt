// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.sargunv.innkeeper.system.documents

import android.database.Cursor
import android.database.MatrixCursor
import android.os.CancellationSignal
import android.os.ParcelFileDescriptor
import android.provider.DocumentsContract.Document
import android.provider.DocumentsContract.Root
import android.provider.DocumentsProvider
import android.webkit.MimeTypeMap
import java.io.FileNotFoundException

/**
 * Host files the user picked through [PickerActivity], as documents. A document id is `<grant>`
 * (the picked file or folder) or `<grant>/<path>` (beneath a picked folder); the bridge resolves it
 * and refuses anything outside what the user picked. There are no roots, so nothing is browsable in
 * DocumentsUI: apps reach documents only through URIs they were granted.
 */
class HostDocumentsProvider : DocumentsProvider() {
  override fun onCreate(): Boolean {
    HostFiles.start()
    return true
  }

  override fun queryRoots(projection: Array<out String>?): Cursor =
    MatrixCursor(projection ?: ROOT_COLUMNS)

  private data class Id(val grant: Long, val path: String) {
    val child
      get() = path.isNotEmpty()
  }

  private fun parse(id: String): Id {
    val hex = id.substringBefore('/')
    val grant = hex.toULongOrNull(16)?.toLong() ?: throw FileNotFoundException("bad document id")
    return Id(grant, if ('/' in id) id.substringAfter('/') else "")
  }

  private fun idOf(grant: Long, path: String) =
    java.lang.Long.toHexString(grant).padStart(16, '0') + if (path.isEmpty()) "" else "/$path"

  private fun mimeOf(m: HostFiles.Meta) =
    if (m.dir) Document.MIME_TYPE_DIR
    else
      MimeTypeMap.getSingleton()
        .getMimeTypeFromExtension(m.name.substringAfterLast('.', "").lowercase())
        ?: "application/octet-stream"

  private fun row(c: MatrixCursor, docId: String, m: HostFiles.Meta, inTree: Boolean) {
    var flags = 0
    if (m.writable) {
      flags =
        flags or if (m.dir) Document.FLAG_DIR_SUPPORTS_CREATE else Document.FLAG_SUPPORTS_WRITE
      // Only things beneath a picked folder can be deleted or renamed.
      if (inTree) flags = flags or Document.FLAG_SUPPORTS_DELETE or Document.FLAG_SUPPORTS_RENAME
    }
    c.newRow()
      .add(Document.COLUMN_DOCUMENT_ID, docId)
      .add(Document.COLUMN_DISPLAY_NAME, m.name)
      .add(Document.COLUMN_MIME_TYPE, mimeOf(m))
      .add(Document.COLUMN_SIZE, if (m.dir) null else m.size)
      .add(Document.COLUMN_LAST_MODIFIED, m.mtimeMs)
      .add(Document.COLUMN_FLAGS, flags)
  }

  override fun queryDocument(documentId: String, projection: Array<out String>?): Cursor {
    val id = parse(documentId)
    val c = MatrixCursor(projection ?: DOCUMENT_COLUMNS)
    row(c, documentId, HostFiles.stat(id.grant, id.path), id.child)
    return c
  }

  override fun queryChildDocuments(
    parentDocumentId: String,
    projection: Array<out String>?,
    sortOrder: String?,
  ): Cursor {
    val id = parse(parentDocumentId)
    val c = MatrixCursor(projection ?: DOCUMENT_COLUMNS)
    for (m in HostFiles.list(id.grant, id.path)) {
      val p = if (id.path.isEmpty()) m.name else "${id.path}/${m.name}"
      row(c, idOf(id.grant, p), m, true)
    }
    return c
  }

  override fun openDocument(
    documentId: String,
    mode: String,
    signal: CancellationSignal?,
  ): ParcelFileDescriptor {
    val id = parse(documentId)
    val m =
      when (mode) {
        "r" -> HostFiles.OPEN_READ
        "w",
        "wt" -> HostFiles.OPEN_WRITE
        "wa" -> HostFiles.OPEN_APPEND
        "rw",
        "rwt" -> HostFiles.OPEN_READ_WRITE
        else -> throw IllegalArgumentException("mode $mode")
      }
    return HostFiles.open(id.grant, id.path, m)
  }

  override fun isChildDocument(parentDocumentId: String, documentId: String): Boolean {
    val p = parse(parentDocumentId)
    val d = parse(documentId)
    return p.grant == d.grant && (p.path.isEmpty() || d.path.startsWith(p.path + "/")) && d.child
  }

  override fun createDocument(
    parentDocumentId: String,
    mimeType: String,
    displayName: String,
  ): String {
    val id = parse(parentDocumentId)
    val dir = mimeType == Document.MIME_TYPE_DIR
    var name = displayName.replace('/', '_')
    // Like DocumentsUI's providers: give files the extension their type implies.
    if (!dir && !name.contains('.')) {
      MimeTypeMap.getSingleton().getExtensionFromMimeType(mimeType)?.let { name = "$name.$it" }
    }
    val made = HostFiles.create(id.grant, id.path, dir, name)
    return idOf(id.grant, if (id.path.isEmpty()) made else "${id.path}/$made")
  }

  override fun deleteDocument(documentId: String) {
    val id = parse(documentId)
    HostFiles.delete(id.grant, id.path)
  }

  override fun renameDocument(documentId: String, displayName: String): String {
    val id = parse(documentId)
    val name = displayName.replace('/', '_')
    HostFiles.rename(id.grant, id.path, name)
    val parent = id.path.substringBeforeLast('/', "")
    return idOf(id.grant, if (parent.isEmpty()) name else "$parent/$name")
  }

  companion object {
    const val AUTHORITY = "dev.sargunv.innkeeper.system.documents"

    private val ROOT_COLUMNS =
      arrayOf(Root.COLUMN_ROOT_ID, Root.COLUMN_FLAGS, Root.COLUMN_TITLE, Root.COLUMN_DOCUMENT_ID)
    private val DOCUMENT_COLUMNS =
      arrayOf(
        Document.COLUMN_DOCUMENT_ID,
        Document.COLUMN_DISPLAY_NAME,
        Document.COLUMN_MIME_TYPE,
        Document.COLUMN_SIZE,
        Document.COLUMN_LAST_MODIFIED,
        Document.COLUMN_FLAGS,
      )

    fun documentId(grant: Long) = java.lang.Long.toHexString(grant).padStart(16, '0')
  }
}
