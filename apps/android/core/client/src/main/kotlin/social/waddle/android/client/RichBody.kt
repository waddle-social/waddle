package social.waddle.android.client

import social.waddle.client.ffi.WaddleMarkupSpan
import social.waddle.client.ffi.WaddleMarkupSpanType
import social.waddle.client.ffi.WaddleReference
import social.waddle.client.ffi.WaddleReferenceType

/**
 * Inbound rich-message model: partitions a display body plus XEP-0394
 * markup spans and XEP-0372 references into renderable blocks, porting
 * the web pipeline (`chat/src/lib/rich-message/parse.ts`) exactly:
 *
 * - offsets are Unicode code points over the WIRE body; when the store
 *   stripped a XEP-0428 reply fallback they rebase past the removed
 *   range, and hostile (attacker-controlled) ranges are validated in
 *   the Long domain and DROPPED, mirroring [mentionSpansIn];
 * - `bcode`/`bquote` spans partition the body into top-level blocks
 *   (a block strictly contained by another is dropped); text between
 *   blocks is trimmed and split into paragraphs on blank lines;
 * - inline styling resolves per offset to the FIRST covering span
 *   (spans sorted start-asc / end-desc), matching the web renderer and
 *   the one-style-per-span shape the Rust parser emits;
 * - hyperlinks come from `type='data'` references (scheme-gated via
 *   [safeUri]) plus bare-URL autolinking with code-range suppression;
 *   XEP-0394 `LINK` markup spans are dropped, exactly like the web's
 *   wasm codec (links ride references, never markup);
 * - received bodies are NEVER markdown-parsed — the web renders
 *   exclusively from markup + references, and so does Android.
 */

/** One styled run of a paragraph. Marks mirror the web's Tiptap marks. */
data class RichInlineRun(
    val text: String,
    val bold: Boolean = false,
    val italic: Boolean = false,
    val strikethrough: Boolean = false,
    val code: Boolean = false,
    /** Scheme-gated hyperlink target ([safeUri]-approved). */
    val linkUri: String? = null,
    /** XEP-0372 mention URI covering this run. */
    val mentionUri: String? = null,
)

/** A rendered block of a rich message body. */
sealed interface RichBlock {
    /** Inline-styled text paragraph; newlines appear as `"\n"` runs. */
    data class Paragraph(val runs: List<RichInlineRun>) : RichBlock

    /** XEP-0394 `bcode`: verbatim monospace block. */
    data class CodeBlock(val text: String) : RichBlock

    /** XEP-0394 `bquote`: quote with `>` line markers stripped. */
    data class Blockquote(val paragraphs: List<Paragraph>) : RichBlock
}

/** Whether any block carries actual rendering beyond plain text. */
fun List<RichBlock>.hasRichContent(): Boolean = any { block ->
    when (block) {
        is RichBlock.CodeBlock, is RichBlock.Blockquote -> true
        is RichBlock.Paragraph -> block.runs.any { run ->
            run.bold || run.italic || run.strikethrough || run.code || run.linkUri != null
        }
    }
}

/**
 * Partition the fallback-STRIPPED display [body] into rich blocks.
 * [fallbackStart]/[fallbackEnd] describe the removed XEP-0428 range in
 * wire code points (matching [stripReplyFallback]); pass `null` when
 * nothing was stripped.
 */
fun richBlocksOf(
    body: String,
    markupSpans: List<WaddleMarkupSpan>,
    references: List<WaddleReference>,
    fallbackStart: UInt?,
    fallbackEnd: UInt?,
): List<RichBlock> {
    if (body.isEmpty()) return emptyList()
    val context = RichParseContext.of(body, markupSpans, references, fallbackStart, fallbackEnd)
    return context.blocksFromRange(0L, context.totalCodePoints)
}

// ── Parse context ────────────────────────────────────────────────────

private data class StyledRange(val type: WaddleMarkupSpanType, val start: Long, val end: Long)

private data class UriRange(val start: Long, val end: Long, val uri: String)

private class RichParseContext(
    val body: String,
    val totalCodePoints: Long,
    val inlineSpans: List<StyledRange>,
    val blockSpans: List<StyledRange>,
    val linkRanges: List<UriRange>,
    val mentionRanges: List<UriRange>,
) {
    companion object {
        fun of(
            body: String,
            markupSpans: List<WaddleMarkupSpan>,
            references: List<WaddleReference>,
            fallbackStart: UInt?,
            fallbackEnd: UInt?,
        ): RichParseContext {
            val totalCodePoints = body.codePointCount(0, body.length).toLong()
            val strip = strippedRangeOf(fallbackStart, fallbackEnd, totalCodePoints)
            val spans = markupSpans
                .mapNotNull { span -> span.rebasedRange(strip, totalCodePoints) }
                .sortedWith(compareBy<StyledRange> { it.start }.thenByDescending { it.end })
            val links = linkRangesOf(body, references, spans, strip, totalCodePoints)
            return RichParseContext(
                body = body,
                totalCodePoints = totalCodePoints,
                inlineSpans = spans.filter { !it.type.isBlock() },
                blockSpans = spans.filter { it.type.isBlock() },
                linkRanges = links,
                mentionRanges = mentionRangesOf(references, strip, totalCodePoints),
            )
        }
    }

    /** Web `blocksFromRange`: top-level blocks with paragraph gaps. */
    fun blocksFromRange(start: Long, end: Long): List<RichBlock> {
        val blocks = mutableListOf<RichBlock>()
        var cursor = start
        for (block in topLevelBlocks(start, end)) {
            appendParagraphs(blocks, cursor, block.start)
            blocks += blockNodeOf(block)
            cursor = block.end
        }
        appendParagraphs(blocks, cursor, end)
        return blocks
    }

    /**
     * Web `topLevelBlockAnnotations`: keep blocks inside `[start, end)`
     * not STRICTLY contained by another; identical ranges both survive
     * (identity comparison, not equality). Sorted start-asc, end-asc.
     */
    private fun topLevelBlocks(start: Long, end: Long): List<StyledRange> {
        val contained = blockSpans.filter { it.start >= start && it.end <= end }
        return contained
            .filterIndexed { index, block ->
                contained.withIndex().none { (otherIndex, other) ->
                    otherIndex != index &&
                        other.start <= block.start && other.end >= block.end &&
                        (other.start < block.start || other.end > block.end)
                }
            }
            .sortedWith(compareBy({ it.start }, { it.end }))
    }

    private fun blockNodeOf(block: StyledRange): RichBlock = when (block.type) {
        WaddleMarkupSpanType.CODE_BLOCK -> RichBlock.CodeBlock(slice(block.start, block.end))
        else -> RichBlock.Blockquote(quoteParagraphs(block))
    }

    /** Web `quoteBlocksFromAnnotation` + `strippedQuoteSegments`. */
    private fun quoteParagraphs(quote: StyledRange): List<RichBlock.Paragraph> {
        val segments = mutableListOf<TextSegment>()
        val lines = slice(quote.start, quote.end).split("\n")
        var lineStart = quote.start
        lines.forEachIndexed { index, line ->
            val marker = when {
                line.startsWith("> ") -> 2
                line.startsWith(">") -> 1
                else -> 0
            }
            if (line.length > marker) {
                segments += TextSegment(line.substring(marker), lineStart + marker)
            }
            val lineCodePoints = line.codePointCount(0, line.length).toLong()
            if (index < lines.size - 1) {
                segments += TextSegment("\n", lineStart + lineCodePoints)
            }
            lineStart += lineCodePoints + 1
        }
        val paragraphs = paragraphsFromSegments(segments)
        return paragraphs.ifEmpty { listOf(RichBlock.Paragraph(emptyList())) }
    }

    /** Web `appendTextBlocks`: trim, then paragraphs from the gap. */
    private fun appendParagraphs(blocks: MutableList<RichBlock>, start: Long, end: Long) {
        if (end <= start) return
        val trimmed = trimWhitespaceRange(start, end) ?: return
        val text = slice(trimmed.first, trimmed.second)
        blocks += paragraphsFromSegments(listOf(TextSegment(text, trimmed.first)))
    }

    private fun trimWhitespaceRange(start: Long, end: Long): Pair<Long, Long>? {
        val codePoints = slice(start, end).codePoints().toArray()
        var left = 0
        var right = codePoints.size
        while (left < right && Character.isWhitespace(codePoints[left])) left++
        while (right > left && Character.isWhitespace(codePoints[right - 1])) right--
        if (right <= left) return null
        return (start + left) to (start + right)
    }

    /** Web `paragraphsFromSegments` + `splitSegmentsOnBlankLines`. */
    private fun paragraphsFromSegments(segments: List<TextSegment>): List<RichBlock.Paragraph> {
        val paragraphs = mutableListOf<RichBlock.Paragraph>()
        var current = mutableListOf<TextSegment>()
        fun flush() {
            if (current.isNotEmpty()) {
                paragraphs += RichBlock.Paragraph(runsForSegments(current))
                current = mutableListOf()
            }
        }
        for (segment in segments) {
            var offset = segment.start
            var lastIndex = 0
            for (match in BLANK_LINES.findAll(segment.text)) {
                val before = segment.text.substring(lastIndex, match.range.first)
                if (before.isNotEmpty()) {
                    current += TextSegment(before, offset)
                    offset += before.codePointCount(0, before.length)
                }
                flush()
                offset += match.value.length.toLong()
                lastIndex = match.range.last + 1
            }
            val tail = segment.text.substring(lastIndex)
            if (tail.isNotEmpty()) current += TextSegment(tail, offset)
        }
        flush()
        return paragraphs
    }

    /** Web `inlineNodesForSegments`: coalesce equal-mark runs, newline resets. */
    private fun runsForSegments(segments: List<TextSegment>): List<RichInlineRun> {
        val builder = RunBuilder()
        for (segment in segments) {
            appendSegmentRuns(builder, segment)
        }
        builder.flush()
        return builder.runs
    }

    private fun appendSegmentRuns(builder: RunBuilder, segment: TextSegment) {
        var offset = segment.start
        var index = 0
        while (index < segment.text.length) {
            val codePoint = segment.text.codePointAt(index)
            if (codePoint == '\n'.code) {
                builder.hardBreak()
            } else {
                builder.append(codePoint, marksAt(offset))
            }
            index += Character.charCount(codePoint)
            offset += 1
        }
    }

    /** Web `marksAtOffset`: first covering span + first covering link. */
    private fun marksAt(offset: Long): RichMarks {
        val span = inlineSpans.firstOrNull { it.start <= offset && offset < it.end }
        return RichMarks(
            bold = span?.type == WaddleMarkupSpanType.BOLD,
            italic = span?.type == WaddleMarkupSpanType.ITALIC,
            strikethrough = span?.type == WaddleMarkupSpanType.STRIKETHROUGH,
            code = span?.type == WaddleMarkupSpanType.CODE,
            linkUri = linkRanges.firstOrNull { it.start <= offset && offset < it.end }?.uri,
            mentionUri = mentionRanges.firstOrNull { it.start <= offset && offset < it.end }?.uri,
        )
    }

    private fun slice(start: Long, end: Long): String {
        val from = body.offsetByCodePoints(0, start.toInt())
        val to = body.offsetByCodePoints(0, end.toInt())
        return body.substring(from, to)
    }
}

private val BLANK_LINES = Regex("\n{2,}")

private data class TextSegment(val text: String, val start: Long)

/** Accumulates code points into mark-coalesced [RichInlineRun]s. */
private class RunBuilder {
    val runs = mutableListOf<RichInlineRun>()
    private val buffer = StringBuilder()
    private var marks: RichMarks? = null

    fun append(codePoint: Int, here: RichMarks) {
        if (here != marks) {
            flush()
            marks = here
        }
        buffer.appendCodePoint(codePoint)
    }

    /** Newline: its own unstyled run, and marks reset (web hardBreak). */
    fun hardBreak() {
        flush()
        runs += RichInlineRun("\n")
        marks = null
    }

    fun flush() {
        if (buffer.isNotEmpty()) {
            runs += (marks ?: RichMarks.PLAIN).runOf(buffer.toString())
            buffer.setLength(0)
        }
    }
}

private data class RichMarks(
    val bold: Boolean,
    val italic: Boolean,
    val strikethrough: Boolean,
    val code: Boolean,
    val linkUri: String?,
    val mentionUri: String?,
) {
    fun runOf(text: String) = RichInlineRun(
        text = text,
        bold = bold,
        italic = italic,
        strikethrough = strikethrough,
        code = code,
        linkUri = linkUri,
        mentionUri = mentionUri,
    )

    companion object {
        val PLAIN = RichMarks(
            bold = false,
            italic = false,
            strikethrough = false,
            code = false,
            linkUri = null,
            mentionUri = null,
        )
    }
}

private fun WaddleMarkupSpanType.isBlock(): Boolean =
    this == WaddleMarkupSpanType.CODE_BLOCK || this == WaddleMarkupSpanType.BLOCKQUOTE

/**
 * Rebase a wire span past the stripped fallback and validate in the
 * Long domain; out-of-range or collapsed spans are dropped (web
 * `isValidInboundMarkup` + `stripMarkupRange` parity). `LINK` markup
 * spans are dropped entirely — hyperlinks ride XEP-0372 references.
 */
private fun WaddleMarkupSpan.rebasedRange(strip: LongRange?, totalCodePoints: Long): StyledRange? {
    if (spanType == WaddleMarkupSpanType.LINK) return null
    val begin = start.toLong()
    val finish = end.toLong()
    if (finish <= begin) return null
    val displayBegin = strip?.let { rebaseAfterRemoval(begin, it) } ?: begin
    val displayEnd = strip?.let { rebaseAfterRemoval(finish, it) } ?: finish
    if (displayEnd <= displayBegin || displayEnd > totalCodePoints) return null
    return StyledRange(spanType, displayBegin, displayEnd)
}

/** Web parse-context references: valid `data` refs + bare-URL autolinks. */
private fun linkRangesOf(
    body: String,
    references: List<WaddleReference>,
    spans: List<StyledRange>,
    strip: LongRange?,
    totalCodePoints: Long,
): List<UriRange> {
    val explicit = references.mapNotNull { reference ->
        if (reference.refType != WaddleReferenceType.Data) return@mapNotNull null
        val uri = safeUri(reference.uri) ?: return@mapNotNull null
        reference.rebasedUriRange(uri, strip, totalCodePoints)
    }
    val codeRanges = spans
        .filter { it.type == WaddleMarkupSpanType.CODE || it.type == WaddleMarkupSpanType.CODE_BLOCK }
        .map { it.start until it.end }
    val inferred = inferBareUrlRanges(
        body = body,
        existingRanges = explicit.map { it.start until it.end },
        codeRanges = codeRanges,
    ).map { UriRange(start = it.start, end = it.end, uri = it.uri) }
    return (explicit + inferred).sortedWith(compareBy({ it.start }, { it.end }))
}

private fun mentionRangesOf(
    references: List<WaddleReference>,
    strip: LongRange?,
    totalCodePoints: Long,
): List<UriRange> = references.mapNotNull { reference ->
    if (reference.refType != WaddleReferenceType.Mention) return@mapNotNull null
    reference.rebasedUriRange(reference.uri, strip, totalCodePoints)
}

private fun WaddleReference.rebasedUriRange(
    uri: String,
    strip: LongRange?,
    totalCodePoints: Long,
): UriRange? {
    val start = begin.toLong()
    val finish = end.toLong()
    // (0, 0) is the anchor-only "no body position" sentinel.
    if (finish <= start) return null
    val displayBegin = strip?.let { rebaseAfterRemoval(start, it) } ?: start
    val displayEnd = strip?.let { rebaseAfterRemoval(finish, it) } ?: finish
    if (displayEnd <= displayBegin || displayEnd > totalCodePoints) return null
    return UriRange(start = displayBegin, end = displayEnd, uri = uri)
}
