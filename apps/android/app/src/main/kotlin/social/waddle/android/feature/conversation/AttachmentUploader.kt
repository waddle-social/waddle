package social.waddle.android.feature.conversation

import android.content.ContentResolver
import android.net.Uri
import android.provider.OpenableColumns
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import okhttp3.OkHttpClient
import social.waddle.android.client.SlotUploader
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.client.dispositionFor
import social.waddle.android.client.prefs.SharedFileRef

/** Outcome of one attachment upload attempt. */
sealed interface UploadResult {
    data class Done(val file: SharedFileRef) : UploadResult

    data object TooLarge : UploadResult

    data object Failed : UploadResult
}

/**
 * XEP-0363 upload of a user-picked document: resolve name/size/type
 * from the content Uri, request a slot through the live session, and
 * stream the bytes to the slot's PUT URL. 10 MB cap (web parity).
 * Uploads are plaintext — the native client has no OMEMO yet, so the
 * web's encrypted-attachment path does not apply.
 */
class AttachmentUploader(
    private val contentResolver: ContentResolver,
    httpClient: OkHttpClient,
    private val sessionManager: XmppSessionManager,
) {
    private val slotUploader = SlotUploader(httpClient)

    suspend fun upload(uri: Uri): UploadResult {
        val meta = withContext(Dispatchers.IO) { resolveMeta(uri) } ?: return UploadResult.Failed
        if (meta.sizeBytes > MAX_UPLOAD_BYTES) return UploadResult.TooLarge
        val contentType = withContext(Dispatchers.IO) {
            runCatching { contentResolver.getType(uri) }.getOrNull()
        } ?: DEFAULT_CONTENT_TYPE
        val slot = sessionManager.requestUploadSlot(
            filename = meta.name,
            sizeBytes = meta.sizeBytes.toULong(),
            contentType = contentType,
        ) ?: return UploadResult.Failed
        val uploaded = slotUploader.put(
            slot = slot,
            contentType = contentType,
            contentLength = meta.sizeBytes,
            open = { contentResolver.openInputStream(uri) },
        )
        if (!uploaded) return UploadResult.Failed
        return UploadResult.Done(
            SharedFileRef(
                url = slot.getUrl,
                name = meta.name,
                mediaType = contentType,
                sizeBytes = meta.sizeBytes,
                disposition = dispositionFor(contentType),
            ),
        )
    }

    private fun resolveMeta(uri: Uri): AttachmentMeta? = runCatching {
        contentResolver.query(uri, null, null, null, null)?.use { cursor ->
            if (!cursor.moveToFirst()) return@use null
            val nameIndex = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
            val sizeIndex = cursor.getColumnIndex(OpenableColumns.SIZE)
            val name = if (nameIndex >= 0) cursor.getString(nameIndex) else null
            val size = if (sizeIndex >= 0 && !cursor.isNull(sizeIndex)) cursor.getLong(sizeIndex) else -1L
            if (size < 0) return@use null
            AttachmentMeta(name = name ?: DEFAULT_FILENAME, sizeBytes = size)
        }
    }.getOrNull()

    private data class AttachmentMeta(val name: String, val sizeBytes: Long)

    private companion object {
        /** Web MAX_UPLOAD_BYTES parity. */
        const val MAX_UPLOAD_BYTES = 10L * 1024 * 1024
        const val DEFAULT_FILENAME = "attachment"
        const val DEFAULT_CONTENT_TYPE = "application/octet-stream"
    }
}
