package social.waddle.android.feature.profile

import android.content.ContentResolver
import android.graphics.Bitmap
import android.graphics.ImageDecoder
import android.net.Uri
import androidx.core.graphics.scale
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.io.ByteArrayOutputStream

/**
 * Decode a picked image and apply the pure [avatarGeometry] transform:
 * center-crop square, downscale to at most [MAX_AVATAR_DIMENSION]
 * (never upscale), re-encode as PNG. `null` on any decode failure —
 * the caller surfaces a "couldn't read image" state.
 */
class BitmapAvatarProcessor(private val contentResolver: ContentResolver) {
    suspend fun process(uri: Uri): ProcessedAvatar? = withContext(Dispatchers.IO) {
        try {
            val bitmap = ImageDecoder.decodeBitmap(
                ImageDecoder.createSource(contentResolver, uri),
            ) { decoder, _, _ ->
                // Software allocation: hardware bitmaps cannot be read
                // back for cropping or PNG encoding.
                decoder.allocator = ImageDecoder.ALLOCATOR_SOFTWARE
            }
            encode(bitmap)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            null
        }
    }

    private fun encode(bitmap: Bitmap): ProcessedAvatar? {
        val geometry = avatarGeometry(bitmap.width, bitmap.height) ?: return null
        val square = Bitmap.createBitmap(bitmap, geometry.cropX, geometry.cropY, geometry.cropSize, geometry.cropSize)
        val scaled = if (geometry.outputSize == geometry.cropSize) {
            square
        } else {
            square.scale(geometry.outputSize, geometry.outputSize)
        }
        val bytes = ByteArrayOutputStream().use { out ->
            scaled.compress(Bitmap.CompressFormat.PNG, 100, out)
            out.toByteArray()
        }
        return ProcessedAvatar(
            data = bytes,
            mimeType = "image/png",
            width = geometry.outputSize,
            height = geometry.outputSize,
        )
    }
}
