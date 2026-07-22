package social.waddle.android.client

import kotlinx.serialization.Serializable

/**
 * Send-time markdown: converts the literal markdown the user typed
 * into a clean body plus XEP-0394 markup spans, producing the same
 * wire shapes the web's WYSIWYG composer serializes
 * (`chat/src/lib/rich-message/serialize.ts`):
 *
 * - `**bold**`, `*italic*`, `~~strike~~`, `` `code` `` become inline
 *   spans with the marker characters removed from the body;
 * - ```` ``` ```` fences become `bcode` blocks — the fence lines are
 *   removed, the code text stays verbatim (the FFI span carries no
 *   language, so any fence info string is dropped);
 * - `>`-prefixed line groups become `bquote` blocks — the `>` markers
 *   STAY in the body (web wire parity; receivers strip them);
 * - lists are NOT markdown (web parity: XEP-0394 lists never hit the
 *   wire);
 * - all offsets are Unicode code points over the final body, and
 *   mention offsets recorded over the raw draft are rebased across
 *   every marker removal.
 *
 * Markers inside code (span or block) are literal. Marker styles do
 * not nest (`***x***` renders bold with literal inner asterisks); the
 * receiving renderer resolves one style per offset anyway.
 */

/** Serializable XEP-0394 span kind for queued offline sends. */
@Serializable
enum class MarkupRefType { BOLD, ITALIC, STRIKETHROUGH, CODE, CODE_BLOCK, BLOCKQUOTE }

/**
 * One outbound markup span: [begin] inclusive / [end] exclusive,
 * Unicode CODE POINTS over the trimmed send body BEFORE any reply
 * fallback prefix (`preparedSend` rebases onto the final wire body,
 * exactly like [MentionRef]).
 */
@Serializable
data class MarkupRef(val type: MarkupRefType, val begin: UInt, val end: UInt)

/** Result of [composeMarkdown]: clean body + spans + rebased mentions. */
data class ComposedMarkdown(
    val body: String,
    val markup: List<MarkupRef>,
    val mentions: List<MentionRef>,
)

/** Convert markdown in the trimmed [body]; [mentions] are rebased. */
fun composeMarkdown(body: String, mentions: List<MentionRef>): ComposedMarkdown {
    val fences = extractFences(body)
    val inline = applyInlineMarkers(fences.body, fences.codeBlocks)
    val codeBlocks = fences.codeBlocks.map { CpRange(inline.mapOffset(it.start), inline.mapOffset(it.end)) }
    val markup = codeBlocks.map { it.toMarkupRef(MarkupRefType.CODE_BLOCK) } +
        inline.spans +
        blockquoteSpans(inline.body, codeBlocks)
    val rebased = mentions.mapNotNull { mention ->
        val begin = inline.mapOffset(fences.mapOffset(mention.begin.toLong()))
        val end = inline.mapOffset(fences.mapOffset(mention.end.toLong()))
        if (end <= begin) null else mention.copy(begin = begin.toUInt(), end = end.toUInt())
    }
    return ComposedMarkdown(
        body = inline.body,
        markup = markup.sortedWith(compareBy({ it.begin }, { it.end })),
        mentions = rebased,
    )
}

// ── Shared primitives ────────────────────────────────────────────────

/** Code-point range, [start] inclusive, [end] exclusive. */
private data class CpRange(val start: Long, val end: Long) {
    fun overlaps(other: CpRange): Boolean = start < other.end && other.start < end
    fun contains(offset: Long): Boolean = offset in start until end
    fun toMarkupRef(type: MarkupRefType) =
        MarkupRef(type = type, begin = start.toUInt(), end = end.toUInt())
}

/** Removal of [length] code points at pre-removal offset [start]. */
private data class Removal(val start: Long, val length: Long)

/** Map a pre-removal offset to its post-removal position. */
private fun List<Removal>.mapOffset(offset: Long): Long {
    var shifted = offset
    for (removal in this) {
        if (offset <= removal.start) break
        shifted -= minOf(removal.length, offset - removal.start)
    }
    return maxOf(0, shifted)
}

// ── Pass A: fenced code blocks ───────────────────────────────────────

private class FencePass(
    val body: String,
    /** Code-block content ranges over [body]. */
    val codeBlocks: List<CpRange>,
    private val removals: List<Removal>,
) {
    fun mapOffset(offset: Long): Long = removals.mapOffset(offset)
}

private fun extractFences(body: String): FencePass {
    val lines = body.split("\n")
    val keep = BooleanArray(lines.size) { true }
    val contentGroups = mutableListOf<IntRange>()
    var opener = -1
    lines.forEachIndexed { index, line ->
        when {
            opener < 0 && line.startsWith("```") -> opener = index
            opener >= 0 && line.trim() == "```" -> {
                keep[opener] = false
                keep[index] = false
                if (index > opener + 1) contentGroups += (opener + 1) until index
                opener = -1
            }
        }
    }
    return assembleFencePass(lines, keep, contentGroups)
}

private fun assembleFencePass(
    lines: List<String>,
    keep: BooleanArray,
    contentGroups: List<IntRange>,
): FencePass {
    val lineCp = lines.map { it.codePointCount(0, it.length).toLong() }
    val oldStarts = LongArray(lines.size)
    for (index in 1 until lines.size) {
        oldStarts[index] = oldStarts[index - 1] + lineCp[index - 1] + 1
    }
    val removals = mutableListOf<Removal>()
    val kept = StringBuilder()
    val newStarts = LongArray(lines.size) { -1L }
    var newOffset = 0L
    lines.forEachIndexed { index, line ->
        if (keep[index]) {
            if (kept.isNotEmpty()) {
                kept.append('\n')
                newOffset += 1
            }
            newStarts[index] = newOffset
            kept.append(line)
            newOffset += lineCp[index]
        } else {
            // The dropped fence line plus the newline that joined it.
            removals += Removal(start = oldStarts[index], length = lineCp[index] + 1)
        }
    }
    val codeBlocks = contentGroups.map { group ->
        CpRange(newStarts[group.first], newStarts[group.last] + lineCp[group.last])
    }
    return FencePass(body = kept.toString(), codeBlocks = codeBlocks, removals = removals)
}

// ── Pass B: inline markers, single left-to-right scan ───────────────

private class InlinePass(
    val body: String,
    val spans: List<MarkupRef>,
    private val removals: List<Removal>,
) {
    fun mapOffset(offset: Long): Long = removals.mapOffset(offset)
}

private class InlineRule(pattern: String, val type: MarkupRefType, val markerLength: Int) {
    val regex = Regex(pattern)
}

/** Priority order at equal positions: code wins, then strike, bold, italic. */
private val INLINE_RULES = listOf(
    InlineRule("`([^`\n]+)`", MarkupRefType.CODE, markerLength = 1),
    InlineRule("~~([^\n]+?)~~", MarkupRefType.STRIKETHROUGH, markerLength = 2),
    // The optional tail group is lazy (`??`) so `**a** mid **b**`
    // yields two spans instead of one greedy span across the middle.
    InlineRule("""\*\*(\S(?:[^\n]*?\S)??)\*\*""", MarkupRefType.BOLD, markerLength = 2),
    InlineRule("""\*(\S(?:[^*\n]*?\S)??)\*""", MarkupRefType.ITALIC, markerLength = 1),
)

private data class InlineMatch(val rule: InlineRule, val match: MatchResult)

private fun applyInlineMarkers(body: String, protectedRanges: List<CpRange>): InlinePass {
    val result = StringBuilder()
    val spans = mutableListOf<MarkupRef>()
    val removals = mutableListOf<Removal>()
    var searchFrom = 0
    var removedSoFar = 0L
    while (searchFrom < body.length) {
        val next = earliestInlineMatch(body, searchFrom, protectedRanges) ?: break
        val match = next.match
        val content = match.groupValues[1]
        val markerCp = next.rule.markerLength.toLong()
        val startCp = body.codePointCount(0, match.range.first).toLong()
        val endCp = body.codePointCount(0, match.range.last + 1).toLong()
        result.append(body, searchFrom, match.range.first)
        result.append(content)
        removals += Removal(start = startCp, length = markerCp)
        removals += Removal(start = endCp - markerCp, length = markerCp)
        val spanStart = startCp - removedSoFar
        val contentCp = content.codePointCount(0, content.length).toLong()
        spans += CpRange(spanStart, spanStart + contentCp).toMarkupRef(next.rule.type)
        removedSoFar += markerCp * 2
        searchFrom = match.range.last + 1
    }
    result.append(body, searchFrom, body.length)
    return InlinePass(body = result.toString(), spans = spans, removals = removals)
}

private fun earliestInlineMatch(
    body: String,
    searchFrom: Int,
    protectedRanges: List<CpRange>,
): InlineMatch? {
    var from = searchFrom
    while (from < body.length) {
        val candidates = INLINE_RULES.mapNotNull { rule ->
            rule.regex.find(body, from)?.let { InlineMatch(rule, it) }
        }
        val best = candidates.minByOrNull { it.match.range.first } ?: return null
        val startCp = body.codePointCount(0, best.match.range.first).toLong()
        val endCp = body.codePointCount(0, best.match.range.last + 1).toLong()
        if (protectedRanges.none { it.overlaps(CpRange(startCp, endCp)) }) return best
        // Protected (inside a code block): retry past this match start.
        from = best.match.range.first + 1
    }
    return null
}

// ── Pass C: blockquote line groups ───────────────────────────────────

private fun blockquoteSpans(body: String, codeBlocks: List<CpRange>): List<MarkupRef> {
    val spans = mutableListOf<MarkupRef>()
    var offset = 0L
    var groupStart = -1L
    var groupEnd = -1L
    fun flush() {
        if (groupStart >= 0) {
            spans += CpRange(groupStart, groupEnd).toMarkupRef(MarkupRefType.BLOCKQUOTE)
            groupStart = -1
        }
    }
    for (line in body.split("\n")) {
        val lineCp = line.codePointCount(0, line.length).toLong()
        val insideCode = codeBlocks.any { it.contains(offset) }
        if (line.startsWith(">") && !insideCode) {
            if (groupStart < 0) groupStart = offset
            groupEnd = offset + lineCp
        } else {
            flush()
        }
        offset += lineCp + 1
    }
    flush()
    return spans
}
