package social.waddle.android.feature.conversation

import social.waddle.android.client.MentionCandidate
import social.waddle.android.client.MentionRef

/** Result of accepting a popover candidate: new text + caret position. */
data class MentionInsertion(
    val text: String,
    val cursor: Int,
)

/**
 * Composer-side mention bookkeeping: spans of accepted `@Nick` labels in
 * char (UTF-16) coordinates of the draft text, kept in sync through
 * edits so the send can emit code-point XEP-0372 offsets over the final
 * body. The ViewModel stays wiring-only; all span math lives here.
 */
class MentionSpanTracker {
    private data class TrackedMention(
        /** Char range of the `@display` label, [start] inclusive. */
        val start: Int,
        val endExclusive: Int,
        val display: String,
        val uri: String,
    )

    private var text: String = ""
    private var spans: List<TrackedMention> = emptyList()

    /**
     * Sync with the latest composer text. Edits are modelled as one
     * contiguous replacement (common prefix/suffix diff — exactly what a
     * text field produces per change): spans before it keep their
     * positions, spans after it shift by the length delta, and spans
     * intersecting it are dropped (the label was damaged).
     */
    fun onTextChanged(newText: String) {
        if (newText == text) return
        val prefix = commonPrefixLength(text, newText)
        val suffix = commonSuffixLength(text, newText, prefix)
        val oldChangedEnd = text.length - suffix
        val delta = newText.length - text.length
        spans = spans.mapNotNull { span ->
            when {
                span.endExclusive <= prefix -> span
                span.start >= oldChangedEnd ->
                    span.copy(start = span.start + delta, endExclusive = span.endExclusive + delta)
                else -> null
            }
        }
        text = newText
    }

    /**
     * Replace the active [token] in [current] with `@display ` and
     * record the label span. Returns `null` when the token no longer
     * fits the text (stale UI state).
     */
    fun insertMention(current: String, token: MentionToken, candidate: MentionCandidate): MentionInsertion? {
        if (token.start < 0 || token.end > current.length || token.start >= token.end) return null
        val label = "@${candidate.display}"
        val newText = current.replaceRange(token.start, token.end, "$label ")
        onTextChanged(newText)
        spans = spans + TrackedMention(
            start = token.start,
            endExclusive = token.start + label.length,
            display = candidate.display,
            uri = candidate.uri,
        )
        return MentionInsertion(text = newText, cursor = token.start + label.length + 1)
    }

    /**
     * The XEP-0372 mention refs of the current draft, with begin/end as
     * Unicode CODE POINTS over the TRIMMED body (the exact string the
     * ViewModel sends). Spans whose label text no longer reads
     * `@display` are dropped — stale bookkeeping must never mislabel an
     * unrelated substring on the wire.
     */
    fun mentionRefs(): List<MentionRef> {
        if (spans.isEmpty()) return emptyList()
        val body = text.trim()
        val leading = text.length - text.trimStart().length
        return spans.mapNotNull { span ->
            val start = span.start - leading
            val end = span.endExclusive - leading
            if (start < 0 || end > body.length || start >= end) return@mapNotNull null
            if (body.substring(start, end) != "@${span.display}") return@mapNotNull null
            MentionRef(
                uri = span.uri,
                begin = body.codePointCount(0, start).toUInt(),
                end = body.codePointCount(0, end).toUInt(),
            )
        }
    }
}

private fun commonPrefixLength(old: String, new: String): Int {
    val max = minOf(old.length, new.length)
    var i = 0
    while (i < max && old[i] == new[i]) i++
    return i
}

private fun commonSuffixLength(old: String, new: String, prefix: Int): Int {
    val max = minOf(old.length, new.length) - prefix
    var i = 0
    while (i < max && old[old.length - 1 - i] == new[new.length - 1 - i]) i++
    return i
}
