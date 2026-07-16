package social.waddle.android.client

import java.io.IOException
import java.io.InputStream
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import okhttp3.MediaType
import okhttp3.MediaType.Companion.toMediaTypeOrNull
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody
import okio.BufferedSink
import okio.source
import social.waddle.client.ffi.WaddleUploadSlot

/**
 * XEP-0363 §5: PUT the file bytes to the slot's URL with exactly the
 * slot-provided headers plus the negotiated Content-Type (web
 * `file-upload.ts` parity). Streaming — the file never loads whole
 * into memory.
 */
class SlotUploader(private val httpClient: OkHttpClient) {
    suspend fun put(
        slot: WaddleUploadSlot,
        contentType: String,
        contentLength: Long,
        open: () -> InputStream?,
    ): Boolean = withContext(Dispatchers.IO) {
        val body = streamingBody(contentType.toMediaTypeOrNull(), contentLength, open)
        val request = Request.Builder()
            .url(slot.putUrl)
            .put(body)
            .header("Content-Type", contentType)
            .apply {
                // XEP-0363 §5.1: only Authorization/Cookie/Expires may be
                // relayed from the slot; anything else (or a header with
                // an injected newline) is discarded.
                slot.putHeaders
                    .filter { it.name.lowercase() in ALLOWED_PUT_HEADERS }
                    .filterNot { '\n' in it.name || '\r' in it.name || '\n' in it.value || '\r' in it.value }
                    .forEach { header(it.name, it.value) }
            }
            .build()
        try {
            httpClient.newCall(request).execute().use { response -> response.isSuccessful }
        } catch (_: IOException) {
            false
        }
    }

    private companion object {
        /** XEP-0363 §5.1 relayable header allowlist (lowercase). */
        val ALLOWED_PUT_HEADERS = setOf("authorization", "cookie", "expires")
    }

    private fun streamingBody(
        mediaType: MediaType?,
        length: Long,
        open: () -> InputStream?,
    ): RequestBody = object : RequestBody() {
        override fun contentType(): MediaType? = mediaType

        override fun contentLength(): Long = length

        override fun writeTo(sink: BufferedSink) {
            val stream = open() ?: throw IOException("attachment stream unavailable")
            stream.source().use(sink::writeAll)
        }
    }
}
