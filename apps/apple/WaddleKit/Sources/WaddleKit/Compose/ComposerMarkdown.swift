import Foundation

// Send-time markdown: converts the literal markdown the user typed into a
// clean body plus XEP-0394 markup spans, producing the same wire shapes as
// Android's `composeMarkdown` and the web's WYSIWYG composer
// (`chat/src/lib/rich-message/serialize.ts`):
//
// - `**bold**`, `*italic*`, `~~strike~~`, `` `code` `` become inline spans
//   with the marker characters removed from the body;
// - ```` ``` ```` fences become code blocks: the fence lines are removed,
//   the code text stays verbatim (the span carries no language, so any
//   fence info string is dropped);
// - `>`-prefixed line groups become blockquotes; the `>` markers STAY in
//   the body (web wire parity; receivers strip them);
// - lists are NOT markdown (XEP-0394 lists never hit the wire);
// - all offsets are Unicode scalars (code points) over the final body, and
//   mention offsets recorded over the raw draft are rebased across every
//   marker removal.
//
// Markers inside code (span or block) are literal. Marker styles do not
// nest (`***x***` renders bold with literal inner asterisks); the receiving
// renderer resolves one style per offset anyway.

public struct ComposedMarkdown: Hashable, Sendable {
    public let body: String
    /// Sorted by (start, end). Kinds used: .bold, .italic, .strikethrough,
    /// .code, .codeBlock, .blockquote (never .link).
    public let spans: [MarkupSpan]
    /// References rebased onto `body`, in Unicode scalar offsets.
    public let references: DraftReferences
}

public enum ComposerMarkdown {
    /// Converts markdown in the trimmed `body` and rebases its references.
    public static func compose(_ body: String, references: DraftReferences = .init()) -> ComposedMarkdown {
        let fences = extractFences(Array(body.unicodeScalars))
        let literal = references.literalRanges(after: fences.removals)
        let inline = applyInlineMarkers(fences.body, protected: fences.codeBlocks, literal: literal)
        let codeBlocks = fences.codeBlocks.map { $0.rebased(across: inline.removals) }
        let spans = codeBlocks.map { $0.span(.codeBlock) }
            + inline.spans
            + blockquoteSpans(inline.body, codeBlocks: codeBlocks)
        return ComposedMarkdown(
            body: String(String.UnicodeScalarView(inline.body)),
            spans: sortedStably(spans),
            references: references.rebased(fences: fences.removals, inline: inline.removals, spans: spans)
        )
    }
}

// MARK: - Shared primitives

typealias Scalars = [Unicode.Scalar]

/// Scalar range, `start` inclusive, `end` exclusive.
struct ScalarRange {
    let start: Int
    let end: Int

    func overlaps(_ other: ScalarRange) -> Bool { start < other.end && other.start < end }
    func contains(_ offset: Int) -> Bool { start <= offset && offset < end }
    func span(_ kind: MarkupSpan.Kind) -> MarkupSpan { MarkupSpan(kind: kind, start: start, end: end) }

    func rebased(across removals: [Removal]) -> ScalarRange {
        ScalarRange(start: removals.mapOffset(start), end: removals.mapOffset(end))
    }
}

/// Removal of `length` scalars at pre-removal offset `start`.
struct Removal {
    let start: Int
    let length: Int
}

extension [Removal] {
    /// Maps a pre-removal offset to its post-removal position. Removals are
    /// ascending by `start`.
    func mapOffset(_ offset: Int) -> Int {
        var shifted = offset
        for removal in self {
            if offset <= removal.start { break }
            shifted -= Swift.min(removal.length, offset - removal.start)
        }
        return Swift.max(0, shifted)
    }
}

func rebase(_ range: Range<Int>, fences: [Removal], inline: [Removal]) -> Range<Int>? {
    let start = inline.mapOffset(fences.mapOffset(range.lowerBound))
    let end = inline.mapOffset(fences.mapOffset(range.upperBound))
    guard end > start else { return nil }
    return start..<end
}

/// Sorts by (start, end), keeping insertion order on ties like Kotlin's
/// stable `sortedWith`.
private func sortedStably(_ spans: [MarkupSpan]) -> [MarkupSpan] {
    spans.enumerated()
        .sorted { lhs, rhs in
            (lhs.element.start, lhs.element.end, lhs.offset) < (rhs.element.start, rhs.element.end, rhs.offset)
        }
        .map(\.element)
}

func hasPrefix(_ scalars: Scalars, _ prefix: Scalars, at index: Int) -> Bool {
    guard index >= 0, index + prefix.count <= scalars.count else { return false }
    return scalars[index..<(index + prefix.count)].elementsEqual(prefix)
}

// MARK: - Pass A: fenced code blocks

private struct FencePass {
    let body: Scalars
    /// Code-block content ranges over `body`.
    let codeBlocks: [ScalarRange]
    let removals: [Removal]
}

private func extractFences(_ body: Scalars) -> FencePass {
    let draft = ComposerDraftLines(body)
    let lines = draft.lines
    let dropped = Set(draft.codeBlocks.flatMap { [$0.opening, $0.closing] })
    var removals: [Removal] = []
    var kept: Scalars = []
    var newStarts = [Int](repeating: 0, count: lines.count)
    for (index, line) in lines.enumerated() {
        let content = line.beforeLF
        if dropped.contains(index) {
            // The dropped fence line plus the newline that joined it.
            removals.append(Removal(start: content.lowerBound, length: content.count + 1))
        } else {
            // Kotlin joins on `kept.isNotEmpty()`, so leading empty kept
            // lines contribute no newline; mirrored for wire parity.
            if !kept.isEmpty {
                kept.append("\n")
            } else if content.isEmpty, index + 1 < lines.count {
                // The skipped blank still consumes one scalar in the draft.
                removals.append(Removal(start: content.lowerBound, length: 1))
            }
            newStarts[index] = kept.count
            kept.append(contentsOf: body[content])
        }
    }
    let codeBlocks = draft.codeBlocks.compactMap { block -> ScalarRange? in
        guard let first = block.contentLines.first, let last = block.contentLines.last else { return nil }
        return ScalarRange(start: newStarts[first], end: newStarts[last] + lines[last].beforeLF.count)
    }
    return FencePass(body: kept, codeBlocks: codeBlocks, removals: removals)
}

func isSeparator(_ scalar: Unicode.Scalar) -> Bool {
    switch scalar.properties.generalCategory {
    case .spaceSeparator, .lineSeparator, .paragraphSeparator: return true
    default: return false
    }
}

// MARK: - Pass C: blockquote line groups

private func blockquoteSpans(_ body: Scalars, codeBlocks: [ScalarRange]) -> [MarkupSpan] {
    var spans: [MarkupSpan] = []
    var group: ScalarRange?
    for line in ComposerDraftLines.split(body) {
        let offset = line.content.lowerBound
        let insideCode = codeBlocks.contains { $0.contains(offset) }
        if body[line.beforeLF].first == ">", !insideCode {
            group = ScalarRange(start: group?.start ?? offset, end: line.beforeLF.upperBound)
        } else if let finished = group {
            spans.append(finished.span(.blockquote))
            group = nil
        }
    }
    if let finished = group { spans.append(finished.span(.blockquote)) }
    return spans
}
