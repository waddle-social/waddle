package social.waddle.android.feature.conversation

import android.content.Context
import android.content.Intent
import androidx.core.content.FileProvider
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import social.waddle.android.client.EncryptedAttachmentDownloader
import social.waddle.client.ffi.WaddleSharedFile
import java.io.File
import java.security.MessageDigest
import java.util.Locale

/**
 * Open a XEP-0448 NON-image attachment: an external viewer pointed at
 * the ciphertext URL would show garbage, so download → verify →
 * decrypt into the app cache and hand the plaintext out through the
 * manifest `FileProvider` (content:// grant, never a raw file path).
 */
class EncryptedAttachmentOpener(
    private val context: Context,
    private val downloader: EncryptedAttachmentDownloader,
) {
    /** Returns `false` when fetch/decrypt fails or no viewer exists. */
    suspend fun open(file: WaddleSharedFile): Boolean {
        val encrypted = file.encrypted ?: return false
        val plaintext = downloader.fetchAndDecrypt(file.url, encrypted, file.size?.toLong())
            ?: return false
        val target = withContext(Dispatchers.IO) { writeToCache(file, plaintext) } ?: return false
        return launchViewer(target, file.mediaType)
    }

    /**
     * Delete every decrypted file. Runs at app start and on sign-out:
     * plaintext must not accumulate at rest for a feature whose point
     * is privacy against the file host, and any FileProvider grants
     * handed to viewers are process-scoped anyway.
     */
    suspend fun clearCache() {
        withContext(Dispatchers.IO) {
            File(context.cacheDir, CACHE_DIR_NAME).deleteRecursively()
        }
    }

    private fun writeToCache(file: WaddleSharedFile, plaintext: ByteArray): File? = runCatching {
        val dir = File(context.cacheDir, CACHE_DIR_NAME).apply { mkdirs() }
        File(dir, cacheFileName(file)).apply { writeBytes(plaintext) }
    }.getOrNull()

    /**
     * The peer-controlled display name is reduced to a safe basename
     * and prefixed with a URL digest so two attachments sharing a name
     * never overwrite each other.
     */
    private fun cacheFileName(file: WaddleSharedFile): String {
        val cleaned = (file.name ?: DEFAULT_NAME)
            .substringAfterLast('/')
            .substringAfterLast('\\')
            .replace(UNSAFE_NAME_CHARS, "_")
        val safe = if (cleaned.isBlank() || cleaned.all { it == '.' }) DEFAULT_NAME else cleaned
        val digest = MessageDigest.getInstance("SHA-256").digest(file.url.toByteArray())
        val prefix = digest.take(DIGEST_PREFIX_BYTES)
            .joinToString("") { String.format(Locale.ROOT, "%02x", it) }
        return "$prefix-$safe"
    }

    private fun launchViewer(target: File, mediaType: String?): Boolean = runCatching {
        val uri = FileProvider.getUriForFile(context, "${context.packageName}.fileprovider", target)
        val intent = Intent(Intent.ACTION_VIEW)
            .setDataAndType(uri, mediaType ?: DEFAULT_MEDIA_TYPE)
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
        context.startActivity(intent)
        true
    }.getOrElse { false }

    private companion object {
        /** Must match `res/xml/file_provider_paths.xml`. */
        const val CACHE_DIR_NAME = "decrypted_attachments"
        const val DEFAULT_NAME = "attachment"
        const val DEFAULT_MEDIA_TYPE = "application/octet-stream"
        const val DIGEST_PREFIX_BYTES = 8
        val UNSAFE_NAME_CHARS = Regex("[^A-Za-z0-9._ -]")
    }
}
