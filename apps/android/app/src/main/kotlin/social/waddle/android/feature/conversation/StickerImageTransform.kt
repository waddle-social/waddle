package social.waddle.android.feature.conversation

/**
 * Pure geometry of the sticker-image downscale: XEP-0449 recommends
 * stickers stay small, so picked images shrink to fit inside
 * [STICKER_MAX_DIMENSION]² preserving aspect ratio — never upscaled.
 * Kept side-effect free (no android.graphics) so the math is JVM-unit
 * testable; the bitmap work lives in [StickerImageProcessor].
 */

/** Longest edge of a published sticker image, in pixels. */
const val STICKER_MAX_DIMENSION = 512

/**
 * The decode plan for one source image: [sampleSize] is the power-of-2
 * `BitmapFactory.inSampleSize` pre-shrink (memory bound), and
 * [width]×[height] is the exact final size a scale pass lands on.
 */
data class StickerTransform(
    val sampleSize: Int,
    val width: Int,
    val height: Int,
)

/**
 * Plan the downscale of a `sourceWidth`×`sourceHeight` image into the
 * [maxDimension]² box; `null` for degenerate (non-positive) sources.
 * Images already inside the box keep their size (sampleSize 1).
 */
fun stickerTransformFor(
    sourceWidth: Int,
    sourceHeight: Int,
    maxDimension: Int = STICKER_MAX_DIMENSION,
): StickerTransform? {
    if (sourceWidth <= 0 || sourceHeight <= 0 || maxDimension <= 0) return null
    val longest = maxOf(sourceWidth, sourceHeight)
    if (longest <= maxDimension) {
        return StickerTransform(sampleSize = 1, width = sourceWidth, height = sourceHeight)
    }
    val scale = maxDimension.toDouble() / longest
    // Round the LONGEST edge exactly onto the box; the other edge
    // rounds proportionally and is clamped away from zero (a 5000×1
    // banner must not collapse to a zero-height bitmap).
    val width = (sourceWidth * scale).toInt().coerceIn(1, maxDimension)
    val height = (sourceHeight * scale).toInt().coerceIn(1, maxDimension)
    return StickerTransform(
        sampleSize = sampleSizeFor(longest, maxDimension),
        width = width,
        height = height,
    )
}

/**
 * Largest power of two that keeps `longest / sampleSize` at or above
 * [maxDimension] — the standard `inSampleSize` heuristic: the decoded
 * bitmap stays ≥ the target so the final scale pass only shrinks.
 */
private fun sampleSizeFor(longest: Int, maxDimension: Int): Int {
    var sampleSize = 1
    while (longest / (sampleSize * 2) >= maxDimension) {
        sampleSize *= 2
    }
    return sampleSize
}
