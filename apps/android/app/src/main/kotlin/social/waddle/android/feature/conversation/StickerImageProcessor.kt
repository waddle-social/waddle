package social.waddle.android.feature.conversation

import android.content.ContentResolver
import android.graphics.Bitmap
import android.graphics.ImageDecoder
import android.net.Uri
import androidx.core.graphics.scale
import kotlinx.coroutines.CancellationException
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
 * Bitmap half of the sticker transform: decode a picked image Uri
 * via [ImageDecoder] (EXIF orientation applied — a portrait JPEG must
 * not publish sideways), shrink it onto the [stickerTransformFor] plan
 * (≤512² preserving aspect), and re-encode as lossy WEBP — small on
 * the wire, alpha preserved, and every sticker lands on one
 * predictable media type regardless of what the picker returned.
 * `null` on any decode failure; the caller surfaces it as a create
 * failure.
 */
class StickerImageProcessor(private val contentResolver: ContentResolver) {
    suspend fun process(uri: Uri): ProcessedStickerImage? = withContext(Dispatchers.IO) {
        try {
            decodeAndEncode(uri)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            null
        }
    }

    private fun decodeAndEncode(uri: Uri): ProcessedStickerImage? {
        var plan: StickerTransform? = null
        val decoded = ImageDecoder.decodeBitmap(
            ImageDecoder.createSource(contentResolver, uri),
        ) { decoder, info, _ ->
            // Software allocation: hardware bitmaps cannot be read
            // back for the scale pass or the WEBP encode.
            decoder.allocator = ImageDecoder.ALLOCATOR_SOFTWARE
            // info.size is the EXIF-oriented size (ImageDecoder
            // applies rotation), so the plan — and the published
            // dimensions — match what the user saw in the picker.
            // The pure-geometry module stays the sample-size source:
            // the decode pre-shrinks but never lands below the target,
            // leaving the exact final size to the scale pass.
            val transform = stickerTransformFor(info.size.width, info.size.height)
            plan = transform
            if (transform != null && transform.sampleSize > 1) {
                decoder.setTargetSampleSize(transform.sampleSize)
            }
        }
        val transform = plan
        if (transform == null) {
            decoded.recycle()
            return null
        }
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
