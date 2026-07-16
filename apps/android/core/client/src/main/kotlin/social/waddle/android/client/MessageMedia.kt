package social.waddle.android.client

/**
 * Inline-media body rules, ported from the web's
 * `chat/src/lib/message-media.ts`: a message whose ENTIRE body is one
 * image URL (or any Giphy media URL — the GIF-picker send shape)
 * renders as an inline image instead of a text bubble.
 */

/** Web `IMAGE_URL_RE`. */
private val IMAGE_URL_PATTERN = Regex(
    """^https?://\S+\.(?:gif|png|jpe?g|webp|avif|bmp|svg)(?:[?#]\S*)?$""",
    RegexOption.IGNORE_CASE,
)

/** Web `GIPHY_URL_RE`: Giphy media hosts serve extension-less GIFs. */
private val GIPHY_URL_PATTERN = Regex(
    """^https?://(?:media\d*\.giphy\.com|i\.giphy\.com)/""",
    RegexOption.IGNORE_CASE,
)

/** Web `isImageUrl`: the trimmed body is a lone image URL. */
fun isImageUrl(body: String): Boolean {
    val trimmed = body.trim()
    return IMAGE_URL_PATTERN.matches(trimmed) || GIPHY_URL_PATTERN.containsMatchIn(trimmed)
}
