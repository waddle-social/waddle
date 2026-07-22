package social.waddle.android.feature.profile

/** Waddle's avatar size convention (web-client parity): published
 *  avatars are square and at most this many pixels per side, never
 *  upscaled. This is a Waddle choice, not an XEP-0084 recommendation —
 *  the XEP only bounds the *encoded* payload, not the dimensions. */
const val MAX_AVATAR_DIMENSION = 512

/** Upper bound on the decoded (pre-crop) dimension: the picker may
 *  hand us a 100-megapixel source, so the decode is subsampled to at
 *  most ~[MAX_DECODE_DIMENSION]² before the crop/scale transform ever
 *  materializes a bitmap. Twice [MAX_AVATAR_DIMENSION] keeps enough
 *  resolution for a quality downscale. */
const val MAX_DECODE_DIMENSION = 1024

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

/**
 * Pure sample-size math for `ImageDecoder.setTargetSampleSize`: the
 * smallest integer divisor that brings the LARGER source dimension
 * within [maxDimension], so at most ~[maxDimension]² pixels are ever
 * materialized by the decode. `1` (full decode) for sources already
 * within bounds or degenerate inputs.
 */
fun avatarDecodeSampleSize(width: Int, height: Int, maxDimension: Int = MAX_DECODE_DIMENSION): Int {
    if (width <= 0 || height <= 0 || maxDimension <= 0) return 1
    val largest = maxOf(width, height)
    return (largest + maxDimension - 1) / maxDimension
}

/** One processed image, ready for the XEP-0084 publish verb. */
class ProcessedAvatar(
    val data: ByteArray,
    val mimeType: String,
    val width: Int,
    val height: Int,
)
