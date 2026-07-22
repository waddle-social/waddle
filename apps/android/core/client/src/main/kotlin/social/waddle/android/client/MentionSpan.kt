package social.waddle.android.client

import social.waddle.client.ffi.WaddleReference
import social.waddle.client.ffi.WaddleReferenceType

/** A mention span over a DISPLAY body, char (UTF-16) indices for styling. */
data class MentionSpan(
    val startIndex: Int,
    val endIndex: Int,
    val uri: String,
)

/**
 * Resolves XEP-0372 mention references to styleable char ranges over the
 * fallback-STRIPPED display [body]. Wire offsets are Unicode code points
 * over the wire body; when the store removed a reply-fallback quote
 * (`replyFallbackStart`/`End`, matching [stripReplyFallback]) the
 * offsets rebase past the removed range (web `stripReferenceRange`
 * parity). Offsets are attacker-controlled raw u32: everything is
 * validated in the Long domain and out-of-range spans are DROPPED, so a
 * hostile reference can mis-style at worst, never crash.
 */
fun mentionSpansIn(
    body: String,
    references: List<WaddleReference>,
    fallbackStart: UInt?,
    fallbackEnd: UInt?,
): List<MentionSpan> {
    if (references.isEmpty()) return emptyList()
    val totalCodePoints = body.codePointCount(0, body.length).toLong()
    val strip = strippedRangeOf(fallbackStart, fallbackEnd, totalCodePoints)
    return references.mapNotNull { reference ->
        if (reference.refType != WaddleReferenceType.Mention) return@mapNotNull null
        val begin = reference.begin.toLong()
        val end = reference.end.toLong()
        // (0, 0) is the anchor-only "no body position" sentinel.
        if (end <= begin) return@mapNotNull null
        val displayBegin = strip?.let { rebaseAfterRemoval(begin, it) } ?: begin
        val displayEnd = strip?.let { rebaseAfterRemoval(end, it) } ?: end
        if (displayEnd <= displayBegin || displayEnd > totalCodePoints) return@mapNotNull null
        MentionSpan(
            startIndex = body.offsetByCodePoints(0, displayBegin.toInt()),
            endIndex = body.offsetByCodePoints(0, displayEnd.toInt()),
            uri = reference.uri,
        )
    }
}

internal fun strippedRangeOf(start: UInt?, end: UInt?, displayCodePoints: Long): LongRange? {
    start ?: return null
    end ?: return null
    val startCp = start.toLong()
    val endCp = end.toLong()
    // Mirror [stripReplyFallback]'s rejection: a range starting past the
    // display body cannot have been removed from the wire body, so the
    // reference offsets are wire offsets and must not rebase.
    return if (endCp > startCp && startCp <= displayCodePoints) startCp until endCp else null
}

/** Web `rebaseOffsetAfterRemoval`: shift an offset past a removed range. */
internal fun rebaseAfterRemoval(offset: Long, removed: LongRange): Long = when {
    offset <= removed.first -> offset
    offset > removed.last -> offset - (removed.last + 1 - removed.first)
    else -> removed.first
}
