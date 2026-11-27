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

private func splitLines(_ scalars: Scalars) -> [Scalars] {
    scalars.split(separator: "\n", omittingEmptySubsequences: false).map(Array.init)
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

private let fence: Scalars = Array("```".unicodeScalars)

private func extractFences(_ body: Scalars) -> FencePass {
    let lines = splitLines(body)
    var keep = [Bool](repeating: true, count: lines.count)
    var contentGroups: [ClosedRange<Int>] = []
    var opener: Int?
    for (index, line) in lines.enumerated() {
        if let open = opener {
            guard closesMarkdownFence(line[...]) else { continue }
            keep[open] = false
            keep[index] = false
            if index > open + 1 { contentGroups.append((open + 1)...(index - 1)) }
            opener = nil
        } else if opensMarkdownFence(line[...]) {
            opener = index
        }
    }
    return assembleFencePass(lines, keep: keep, contentGroups: contentGroups)
}

private func assembleFencePass(
    _ lines: [Scalars], keep: [Bool], contentGroups: [ClosedRange<Int>]
) -> FencePass {
    var removals: [Removal] = []
    var kept: Scalars = []
    var newStarts = [Int](repeating: -1, count: lines.count)
    var oldStart = 0
    for (index, line) in lines.enumerated() {
        if keep[index] {
            // Kotlin joins on `kept.isNotEmpty()`, so leading empty kept
            // lines contribute no newline; mirrored for wire parity.
            if !kept.isEmpty {
                kept.append("\n")
            } else if line.isEmpty, index + 1 < lines.count {
                // The skipped blank still consumes one scalar in the draft.
                removals.append(Removal(start: oldStart, length: 1))
            }
            newStarts[index] = kept.count
            kept.append(contentsOf: line)
        } else {
            // The dropped fence line plus the newline that joined it.
            removals.append(Removal(start: oldStart, length: line.count + 1))
        }
        oldStart += line.count + 1
    }
    let codeBlocks = contentGroups.map { group in
        ScalarRange(start: newStarts[group.lowerBound], end: newStarts[group.upperBound] + lines[group.upperBound].count)
    }
    return FencePass(body: kept, codeBlocks: codeBlocks, removals: removals)
}

func opensMarkdownFence(_ line: ArraySlice<Unicode.Scalar>) -> Bool {
    line.starts(with: fence)
}

func closesMarkdownFence(_ line: ArraySlice<Unicode.Scalar>) -> Bool {
    let trimmed = line.drop(while: isKotlinWhitespace)
    return opensMarkdownFence(trimmed) && trimmed.dropFirst(fence.count).allSatisfy(isKotlinWhitespace)
}

/// Kotlin `CharSequence.trim()`: `Character.isWhitespace || isSpaceChar`.
private func isKotlinWhitespace(_ scalar: Unicode.Scalar) -> Bool {
    (0x09...0x0D).contains(scalar.value) || (0x1C...0x1F).contains(scalar.value) || isSeparator(scalar)
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
    var offset = 0
    var group: ScalarRange?
    for line in splitLines(body) {
        let insideCode = codeBlocks.contains { $0.contains(offset) }
        if line.first == ">", !insideCode {
            group = ScalarRange(start: group?.start ?? offset, end: offset + line.count)
        } else if let finished = group {
            spans.append(finished.span(.blockquote))
            group = nil
        }
        offset += line.count + 1
    }
    if let finished = group { spans.append(finished.span(.blockquote)) }
    return spans
}
