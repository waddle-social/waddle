package social.waddle.android.media

import social.waddle.android.client.FileDisposition
import social.waddle.android.client.encryptedAttachmentCacheKey
import social.waddle.client.ffi.WaddleEncryptedFile
import social.waddle.client.ffi.WaddleSharedFile

/**
 * Typed Coil model for a XEP-0448 encrypted image: the outer XEP-0447
 * URL plus the decryption envelope. Handled by
 * [EncryptedAttachmentFetcher]; memory-cached under [cacheKey] by
 * [EncryptedAttachmentKeyer].
 */
data class EncryptedFileModel(
    val url: String,
    val mediaType: String?,
    /** Declared plaintext `<size/>` — the decrypt truncation bound. */
    val sizeBytes: Long?,
    val encrypted: WaddleEncryptedFile,
) {
    /**
     * The web's composite decryption cache key — distinct envelopes
     * for the same URL never collide in the memory cache.
     */
    val cacheKey: String = encryptedAttachmentCacheKey(url, encrypted)
}

/**
 * The Coil model for a shared file's image content: the plain URL for
 * plaintext files, the typed [EncryptedFileModel] when a XEP-0448
 * envelope is attached (never hand Coil a ciphertext URL directly).
 */
fun imageModelOf(file: WaddleSharedFile): Any {
    val encrypted = file.encrypted ?: return file.url
    return EncryptedFileModel(
        url = file.url,
        mediaType = file.mediaType,
        sizeBytes = file.size?.toLong(),
        encrypted = encrypted,
    )
}

/** True for an inline image attachment — encrypted or not. */
fun isInlineImage(file: WaddleSharedFile): Boolean =
    FileDisposition.fromWire(file.disposition) == FileDisposition.INLINE &&
        file.mediaType?.startsWith("image/") == true
