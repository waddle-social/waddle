package social.waddle.android.feature.conversation

import android.content.ContentResolver
import android.net.Uri
import okhttp3.OkHttpClient
import social.waddle.android.client.SlotUploader
import social.waddle.android.client.StickerHash
import social.waddle.android.client.StickerItem
import social.waddle.android.client.VerbResult
import social.waddle.android.client.XmppSessionManager
import java.io.ByteArrayInputStream
import java.security.MessageDigest
import java.util.Base64

/** One picked image + its user-entered lang-less desc (may be blank). */
data class StickerImageInput(
    val uri: Uri,
    val desc: String,
)

/** Outcome of one create-pack attempt. */
sealed interface CreateStickerPackResult {
    data object Ok : CreateStickerPackResult

    data object NotConnected : CreateStickerPackResult

    /** An image failed to process/upload, or the publish was refused. */
    data object Failed : CreateStickerPackResult
}

/**
 * The create-pack pipeline: each image is downscaled to ≤512² WEBP
 * ([StickerImageProcessor]), uploaded over the existing XEP-0363 slot
 * path, and hashed (SHA-256 over the uploaded bytes) into the pack
 * item's XEP-0300 hash; the assembled pack then publishes over the FFI
 * (which recomputes the pack id from content). Any per-image failure
 * aborts the whole create — a pack missing stickers the user picked
 * would publish silently-wrong content.
 */
class StickerPackCreator(
    contentResolver: ContentResolver,
    httpClient: OkHttpClient,
    private val sessionManager: XmppSessionManager,
) {
    private val processor = StickerImageProcessor(contentResolver)
    private val slotUploader = SlotUploader(httpClient)

    suspend fun create(
        name: String,
        summary: String?,
        images: List<StickerImageInput>,
        onProgress: (uploaded: Int, total: Int) -> Unit = { _, _ -> },
    ): CreateStickerPackResult {
        val packName = name.trim()
        if (packName.isEmpty() || images.isEmpty()) return CreateStickerPackResult.Failed
        // The account this attempt belongs to: uploads are plain HTTP
        // that keep running across a logout/relogin, and the publish
        // must never land in ANOTHER account's PEP node.
        val owner = sessionManager.ownFullJid() ?: return CreateStickerPackResult.NotConnected
        val items = mutableListOf<StickerItem>()
        images.forEachIndexed { index, input ->
            onProgress(index, images.size)
            val item = uploadSticker(index, input, packName) ?: return CreateStickerPackResult.Failed
            items += item
        }
        onProgress(images.size, images.size)
        if (sessionManager.ownFullJid() != owner) return CreateStickerPackResult.NotConnected
        return when (sessionManager.publishStickerPack(packName, summary, items)) {
            VerbResult.Ok -> CreateStickerPackResult.Ok
            VerbResult.NotConnected -> CreateStickerPackResult.NotConnected
            else -> CreateStickerPackResult.Failed
        }
    }

    private suspend fun uploadSticker(
        index: Int,
        input: StickerImageInput,
        packName: String,
    ): StickerItem? {
        val processed = processor.process(input.uri) ?: return null
        val slot = sessionManager.requestUploadSlot(
            filename = "sticker-${index + 1}.webp",
            sizeBytes = processed.bytes.size.toULong(),
            contentType = processed.mediaType,
        ) ?: return null
        val uploaded = slotUploader.put(
            slot = slot,
            contentType = processed.mediaType,
            contentLength = processed.bytes.size.toLong(),
            open = { ByteArrayInputStream(processed.bytes) },
        )
        if (!uploaded) return null
        return StickerItem(
            // XEP-0446 desc is mandatory and lang-less; an empty user
            // entry falls back to the pack name so the fallback body of
            // a send is never blank.
            desc = input.desc.trim().ifEmpty { packName },
            mediaType = processed.mediaType,
            sizeBytes = processed.bytes.size.toLong(),
            width = processed.width,
            height = processed.height,
            hashes = listOf(sha256HashOf(processed.bytes)),
            sources = listOf(slot.getUrl),
            suggests = emptyList(),
        )
    }

    /** XEP-0300 `sha-256` hash of the uploaded plaintext bytes. */
    private fun sha256HashOf(bytes: ByteArray): StickerHash = StickerHash(
        algo = "sha-256",
        valueB64 = Base64.getEncoder().encodeToString(
            MessageDigest.getInstance("SHA-256").digest(bytes),
        ),
    )
}
