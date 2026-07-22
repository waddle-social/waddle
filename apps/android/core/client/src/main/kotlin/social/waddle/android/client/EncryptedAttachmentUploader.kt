package social.waddle.android.client

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.withContext
import okhttp3.OkHttpClient
import social.waddle.android.client.prefs.SharedFileRef
import social.waddle.client.ffi.WaddleUploadSlot
import java.io.File
import java.io.InputStream

/** Outcome of one attachment upload attempt. */
sealed interface UploadResult {
    data class Done(val file: SharedFileRef) : UploadResult

    data object TooLarge : UploadResult

    data object Failed : UploadResult
}

/**
 * XEP-0448 encrypt-then-upload pipeline (web parity: attachment
 * encryption is unconditional — transport privacy against the file
 * host, not OMEMO): stage the AES-256-GCM ciphertext on disk, request
 * a XEP-0363 slot for the CIPHERTEXT length under `<name>.enc` /
 * `application/octet-stream`, stream the ciphertext to the PUT URL,
 * and return a [SharedFileRef] carrying the ORIGINAL name/type/size,
 * the plaintext sha-256 (XEP-0448 requires it in the file metadata),
 * and the decryption envelope.
 */
class EncryptedAttachmentUploader(
    httpClient: OkHttpClient,
    private val requestSlot: suspend (filename: String, sizeBytes: ULong, contentType: String) -> WaddleUploadSlot?,
    private val tempDir: File,
    private val encryptor: AttachmentEncryptor = AttachmentEncryptor(),
) {
    private val slotUploader = SlotUploader(httpClient)

    suspend fun upload(
        name: String,
        declaredSize: Long,
        mediaType: String,
        open: () -> InputStream?,
    ): UploadResult {
        // Web parity: the 10 MB cap applies to the PLAINTEXT, before
        // encryption adds the GCM tag.
        if (declaredSize > MAX_UPLOAD_BYTES) return UploadResult.TooLarge
        val staged = withContext(Dispatchers.IO) { encryptor.encryptToFile(tempDir, open) }
            ?: return UploadResult.Failed
        try {
            // Providers may under-report their size column; the streamed
            // length is authoritative for the cap.
            if (staged.plaintextLength > MAX_UPLOAD_BYTES) return UploadResult.TooLarge
            val slot = requestSlot(encryptedUploadName(name), staged.ciphertextLength.toULong(), OCTET_STREAM)
                ?: return UploadResult.Failed
            val uploaded = slotUploader.put(
                slot = slot,
                contentType = OCTET_STREAM,
                contentLength = staged.ciphertextLength,
                open = { staged.cipherFile.inputStream() },
            )
            if (!uploaded) return UploadResult.Failed
            return UploadResult.Done(
                SharedFileRef(
                    url = slot.getUrl,
                    name = name,
                    mediaType = mediaType,
                    sizeBytes = staged.plaintextLength,
                    disposition = FileDisposition.forMediaType(mediaType),
                    hashes = staged.plaintextHashes,
                    encrypted = staged.encrypted.copy(sources = listOf(slot.getUrl)),
                ),
            )
        } finally {
            withContext(NonCancellable + Dispatchers.IO) { staged.cipherFile.delete() }
        }
    }

    companion object {
        /** Web MAX_UPLOAD_BYTES parity (plaintext, pre-encryption). */
        const val MAX_UPLOAD_BYTES = 10L * 1024 * 1024
        private const val OCTET_STREAM = "application/octet-stream"

        /** Web `encryptedUploadName` parity. */
        fun encryptedUploadName(name: String): String =
            if (name.endsWith(".enc")) name else "$name.enc"
    }
}
