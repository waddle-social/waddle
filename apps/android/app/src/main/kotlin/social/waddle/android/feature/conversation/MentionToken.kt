package social.waddle.android.feature.conversation

import social.waddle.android.client.MentionCandidate
import java.text.Normalizer

/**
 * The `@token` being completed at the cursor: char (UTF-16) indices over
 * the composer text; [start] points at the `@`, [end] is the cursor.
 */
data class MentionToken(
    val start: Int,
    val end: Int,
    val query: String,
)

/**
 * The active `@` autocomplete token under a collapsed cursor, or `null`.
 * Web parity: the text before the cursor must end in `@word` preceded by
 * start-of-text or whitespace (`(?:^|\s)@(\S*)$`), so `a@b` never arms
 * the popover.
 */
fun activeMentionToken(text: String, cursor: Int): MentionToken? {
    if (cursor < 0 || cursor > text.length) return null
    val match = ACTIVE_MENTION_TOKEN.find(text.substring(0, cursor)) ?: return null
    val query = match.groupValues[1]
    return MentionToken(start = cursor - query.length - 1, end = cursor, query = query)
}

/**
 * Candidate filter for the popover (web `mentionResults` parity):
 * case-insensitive contains, with a diacritics-folded fallback, capped
 * at [MENTION_RESULT_LIMIT] rows.
 */
fun filterMentionCandidates(
    candidates: List<MentionCandidate>,
    query: String,
): List<MentionCandidate> {
    if (query.isEmpty()) return candidates.take(MENTION_RESULT_LIMIT)
    val lower = query.lowercase()
    val folded = foldDiacritics(query)
    return candidates
        .filter { candidate ->
            candidate.display.lowercase().contains(lower) ||
                foldDiacritics(candidate.display).contains(folded)
        }
        .take(MENTION_RESULT_LIMIT)
}

private fun foldDiacritics(value: String): String =
    Normalizer.normalize(value, Normalizer.Form.NFD)
        .replace(COMBINING_MARKS, "")
        .lowercase()

private val ACTIVE_MENTION_TOKEN = Regex("""(?:^|\s)@(\S*)$""")
private val COMBINING_MARKS = Regex("[\\u0300-\\u036f]")
private const val MENTION_RESULT_LIMIT = 8
