package social.waddle.android.feature.conversation

import android.content.ContentResolver
import android.net.Uri
import android.provider.OpenableColumns
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import okhttp3.OkHttpClient
import social.waddle.android.client.EncryptedAttachmentUploader
import social.waddle.android.client.UploadResult
import social.waddle.android.client.XmppSessionManager
import java.io.File

/**
 * XEP-0363 upload of a user-picked document: resolve name/size/type
 * from the content Uri, then hand the stream to the shared XEP-0448
 * encrypt-then-upload pipeline (web parity: attachments are ALWAYS
 * encrypted — transport privacy against the file host). 10 MB
 * plaintext cap, checked before encryption (web parity).
 */
class AttachmentUploader(
    private val contentResolver: ContentResolver,
    httpClient: OkHttpClient,
    sessionManager: XmppSessionManager,
    cacheDir: File,
) {
    private val pipeline = EncryptedAttachmentUploader(
        httpClient = httpClient,
        requestSlot = sessionManager::requestUploadSlot,
        tempDir = cacheDir,
    )

    suspend fun upload(uri: Uri): UploadResult {
        val meta = withContext(Dispatchers.IO) { resolveMeta(uri) } ?: return UploadResult.Failed
        val contentType = withContext(Dispatchers.IO) {
            runCatching { contentResolver.getType(uri) }.getOrNull()
        } ?: DEFAULT_CONTENT_TYPE
        return pipeline.upload(
            name = meta.name,
            declaredSize = meta.sizeBytes,
            mediaType = contentType,
        ) { contentResolver.openInputStream(uri) }
    }

    private fun resolveMeta(uri: Uri): AttachmentMeta? = runCatching {
        var name: String? = null
        var size = -1L
        contentResolver.query(uri, null, null, null, null)?.use { cursor ->
            if (cursor.moveToFirst()) {
                val nameIndex = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                val sizeIndex = cursor.getColumnIndex(OpenableColumns.SIZE)
                if (nameIndex >= 0) name = cursor.getString(nameIndex)
                if (sizeIndex >= 0 && !cursor.isNull(sizeIndex)) size = cursor.getLong(sizeIndex)
            }
        }
        if (size < 0) {
            // OpenableColumns.SIZE is optional — several real providers
            // (cloud documents, mail attachments) omit it while the
            // stream opens fine. Fall back to the descriptor length.
            size = contentResolver.openAssetFileDescriptor(uri, "r")?.use { it.length } ?: -1L
        }
        if (size < 0) return@runCatching null
        AttachmentMeta(name = name ?: DEFAULT_FILENAME, sizeBytes = size)
    }.getOrNull()

    private data class AttachmentMeta(val name: String, val sizeBytes: Long)

    private companion object {
        const val DEFAULT_FILENAME = "attachment"
        const val DEFAULT_CONTENT_TYPE = "application/octet-stream"
    }
}
