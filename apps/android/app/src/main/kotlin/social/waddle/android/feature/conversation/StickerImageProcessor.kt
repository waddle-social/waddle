package social.waddle.android.feature.conversation

import android.content.ContentResolver
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.net.Uri
import androidx.core.graphics.scale
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.io.ByteArrayOutputStream

/** One sticker image ready to upload: WEBP bytes + final dimensions. */
class ProcessedStickerImage(
    val bytes: ByteArray,
    val width: Int,
    val height: Int,
) {
    val mediaType: String get() = STICKER_MEDIA_TYPE
}

/**
 * Bitmap half of the sticker transform: decode a picked image Uri,
 * shrink it onto the [stickerTransformFor] plan (≤512² preserving
 * aspect), and re-encode as lossy WEBP — small on the wire, alpha
 * preserved, and every sticker lands on one predictable media type
 * regardless of what the picker returned. `null` on any decode
 * failure; the caller surfaces it as a create failure.
 */
class StickerImageProcessor(private val contentResolver: ContentResolver) {
    suspend fun process(uri: Uri): ProcessedStickerImage? = withContext(Dispatchers.IO) {
        runCatching { decodeAndEncode(uri) }.getOrNull()
    }

    private fun decodeAndEncode(uri: Uri): ProcessedStickerImage? {
        val bounds = decodeBounds(uri) ?: return null
        val transform = stickerTransformFor(bounds.first, bounds.second) ?: return null
        val decoded = decodeSampled(uri, transform.sampleSize) ?: return null
        val scaled = scaleOnto(decoded, transform)
        val output = ByteArrayOutputStream()
        val compressed =
            scaled.compress(Bitmap.CompressFormat.WEBP_LOSSY, STICKER_WEBP_QUALITY, output)
        if (scaled !== decoded) scaled.recycle()
        decoded.recycle()
        if (!compressed) return null
        return ProcessedStickerImage(
            bytes = output.toByteArray(),
            width = transform.width,
            height = transform.height,
        )
    }

    private fun decodeBounds(uri: Uri): Pair<Int, Int>? {
        val options = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        contentResolver.openInputStream(uri)?.use { stream ->
            BitmapFactory.decodeStream(stream, null, options)
        } ?: return null
        if (options.outWidth <= 0 || options.outHeight <= 0) return null
        return options.outWidth to options.outHeight
    }

    private fun decodeSampled(uri: Uri, sampleSize: Int): Bitmap? {
        val options = BitmapFactory.Options().apply { inSampleSize = sampleSize }
        return contentResolver.openInputStream(uri)?.use { stream ->
            BitmapFactory.decodeStream(stream, null, options)
        }
    }

    private fun scaleOnto(decoded: Bitmap, transform: StickerTransform): Bitmap =
        if (decoded.width == transform.width && decoded.height == transform.height) {
            decoded
        } else {
            decoded.scale(transform.width, transform.height)
        }

    private companion object {
        const val STICKER_WEBP_QUALITY = 90
    }
}

const val STICKER_MEDIA_TYPE = "image/webp"
