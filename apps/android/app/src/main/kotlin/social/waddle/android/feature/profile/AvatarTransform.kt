package social.waddle.android.feature.profile

/** XEP-0084 recommended upper bound: published avatars are square and
 *  at most this many pixels per side (web parity; never upscaled). */
const val MAX_AVATAR_DIMENSION = 512

/**
 * Geometry of one avatar transform: crop the centered [cropSize]²
 * square at ([cropX], [cropY]), then scale it to [outputSize]².
 */
data class AvatarGeometry(
    val cropX: Int,
    val cropY: Int,
    val cropSize: Int,
    val outputSize: Int,
)

/**
 * Pure transform math for a source image of [width]×[height]:
 * center-crop to the largest square, then downscale to at most
 * [maxDimension] per side — never upscale a smaller source. `null`
 * for degenerate (empty) inputs.
 */
fun avatarGeometry(width: Int, height: Int, maxDimension: Int = MAX_AVATAR_DIMENSION): AvatarGeometry? {
    if (width <= 0 || height <= 0 || maxDimension <= 0) return null
    val cropSize = minOf(width, height)
    return AvatarGeometry(
        cropX = (width - cropSize) / 2,
        cropY = (height - cropSize) / 2,
        cropSize = cropSize,
        outputSize = minOf(cropSize, maxDimension),
    )
}

/** One processed image, ready for the XEP-0084 publish verb. */
class ProcessedAvatar(
    val data: ByteArray,
    val mimeType: String,
    val width: Int,
    val height: Int,
)
