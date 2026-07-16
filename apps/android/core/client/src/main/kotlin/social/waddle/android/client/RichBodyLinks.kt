package social.waddle.android.client

import java.net.URI

/**
 * Link helpers for the rich-body pipeline, ported from the web's
 * `chat/src/lib/rich-message/urls.ts` so both clients hyperlink the
 * same ranges with the same scheme gating.
 */

/** Web `BARE_URL_PATTERN`: conservative bare-URL matcher. */
private val BARE_URL_PATTERN = Regex("""\bhttps?://[^\s<>"'`]+""", RegexOption.IGNORE_CASE)

private const val SENTENCE_PUNCTUATION = ".,;:!?'\""

private val CLOSING_PAIRS = listOf(')' to '(', ']' to '[', '}' to '{')

/**
 * Web `safeUri` parity: only `http:`, `https:`, and `mailto:` URIs may
 * become taps/anchors; everything else (`javascript:`, `data:`,
 * unparsable text) yields `null` and renders as plain text.
 */
fun safeUri(uri: String): String? {
    val scheme = runCatching { URI(uri).scheme }.getOrNull() ?: return null
    return when (scheme.lowercase()) {
        "http", "https", "mailto" -> uri
        else -> null
    }
}

/** A hyperlink range over the display body, Unicode code points. */
internal data class InferredLinkRange(val start: Long, val end: Long, val uri: String)

/**
 * Web `inferBareUrlReferences`: autolink bare URLs in [body], skipping
 * ranges already covered by explicit references ([existingRanges]) and
 * code ranges ([codeRanges]). Returned offsets are code points.
 */
internal fun inferBareUrlRanges(
    body: String,
    existingRanges: List<LongRange>,
    codeRanges: List<LongRange>,
): List<InferredLinkRange> {
    val claimed = existingRanges.toMutableList()
    val inferred = mutableListOf<InferredLinkRange>()
    for (match in BARE_URL_PATTERN.findAll(body)) {
        val candidate = linkCandidateOf(body, match, claimed, codeRanges) ?: continue
        inferred += candidate
        claimed += candidate.start until candidate.end
    }
    return inferred
}

private fun linkCandidateOf(
    body: String,
    match: MatchResult,
    claimed: List<LongRange>,
    codeRanges: List<LongRange>,
): InferredLinkRange? {
    val index = match.range.first
    // Web parity: skip markdown-style `](url` constructs.
    if (index >= 2 && body.substring(index - 2, index) == "](") return null
    val raw = stripUrlTrailingPunctuation(match.value)
    if (raw.isEmpty()) return null
    val uri = safeUri(raw) ?: return null
    val begin = body.codePointCount(0, index).toLong()
    val end = begin + raw.codePointCount(0, raw.length)
    val range = begin until end
    if (claimed.any { it.overlaps(range) } || codeRanges.any { it.overlaps(range) }) return null
    return InferredLinkRange(start = begin, end = end, uri = uri)
}

private fun LongRange.overlaps(other: LongRange): Boolean =
    first <= other.last && other.first <= last

/**
 * Web `stripUrlTrailingPunctuation`: drop sentence-terminal punctuation
 * and *unbalanced* closing brackets, keeping balanced ones (Wikipedia
 * `Foo_(disambiguation)` style URLs stay intact).
 */
internal fun stripUrlTrailingPunctuation(url: String): String {
    var result = url
    while (result.isNotEmpty()) {
        result = trimOneTrailingCharacter(result) ?: break
    }
    return result
}

private fun trimOneTrailingCharacter(value: String): String? {
    val last = value.last()
    if (last in SENTENCE_PUNCTUATION) return value.dropLast(1)
    val pair = CLOSING_PAIRS.firstOrNull { it.first == last } ?: return null
    val unbalanced = value.count { it == pair.first } > value.count { it == pair.second }
    return if (unbalanced) value.dropLast(1) else null
}
