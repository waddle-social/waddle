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

/// Who a mention addresses (XEP-0372 URI shapes shared with web/Android).
public enum MentionTarget: Hashable, Sendable {
    case user(BareJID)
    case everyone
    case here

    /// `xmpp:<barejid>`, or the literal broadcast URIs `xmpp:@everyone` / `xmpp:@here`.
    public var uri: String {
        switch self {
        case let .user(jid): return "xmpp:\(jid)"
        case .everyone: return "xmpp:@everyone"
        case .here: return "xmpp:@here"
        }
    }
}

/// A mention the composer inserted: its target and its range over the raw
/// draft, in Unicode scalars (code points), end exclusive.
public struct MentionDraft: Hashable, Sendable {
    public let target: MentionTarget
    public var range: Range<Int>

    public init(target: MentionTarget, range: Range<Int>) {
        self.target = target
        self.range = range
    }
}

public struct ComposedMarkdown: Hashable, Sendable {
    public let body: String
    /// Sorted by (start, end). Kinds used: .bold, .italic, .strikethrough,
    /// .code, .codeBlock, .blockquote (never .link).
    public let spans: [MarkupSpan]
    /// Mentions rebased onto `body`; mentions that collapse to empty are dropped.
    public let mentions: [MentionDraft]
}

public enum ComposerMarkdown {
    /// Converts markdown in the trimmed `body`; `mentions` are rebased.
    public static func compose(_ body: String, mentions: [MentionDraft]) -> ComposedMarkdown {
        let fences = extractFences(Array(body.unicodeScalars))
        let inline = applyInlineMarkers(fences.body, protected: fences.codeBlocks)
        let codeBlocks = fences.codeBlocks.map {
            ScalarRange(start: inline.removals.mapOffset($0.start), end: inline.removals.mapOffset($0.end))
        }
        let spans = codeBlocks.map { $0.span(.codeBlock) }
            + inline.spans
            + blockquoteSpans(inline.body, codeBlocks: codeBlocks)
        return ComposedMarkdown(
            body: String(String.UnicodeScalarView(inline.body)),
            spans: sortedStably(spans),
            mentions: mentions.compactMap { rebase($0, fences: fences.removals, inline: inline.removals) }
        )
    }
}

// MARK: - Shared primitives

private typealias Scalars = [Unicode.Scalar]

/// Scalar range, `start` inclusive, `end` exclusive.
private struct ScalarRange {
    let start: Int
    let end: Int

    func overlaps(_ other: ScalarRange) -> Bool { start < other.end && other.start < end }
    func contains(_ offset: Int) -> Bool { start <= offset && offset < end }
    func span(_ kind: MarkupSpan.Kind) -> MarkupSpan { MarkupSpan(kind: kind, start: start, end: end) }
}

/// Removal of `length` scalars at pre-removal offset `start`.
private struct Removal {
    let start: Int
    let length: Int
}

extension [Removal] {
    /// Maps a pre-removal offset to its post-removal position. Removals are
    /// ascending by `start`.
    fileprivate func mapOffset(_ offset: Int) -> Int {
        var shifted = offset
        for removal in self {
            if offset <= removal.start { break }
            shifted -= Swift.min(removal.length, offset - removal.start)
        }
        return Swift.max(0, shifted)
    }
}

private func rebase(_ mention: MentionDraft, fences: [Removal], inline: [Removal]) -> MentionDraft? {
    let start = inline.mapOffset(fences.mapOffset(mention.range.lowerBound))
    let end = inline.mapOffset(fences.mapOffset(mention.range.upperBound))
    guard end > start else { return nil }
    return MentionDraft(target: mention.target, range: start..<end)
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

private func hasPrefix(_ scalars: Scalars, _ prefix: Scalars, at index: Int) -> Bool {
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
            guard trimmedKotlinStyle(line) == fence else { continue }
            keep[open] = false
            keep[index] = false
            if index > open + 1 { contentGroups.append((open + 1)...(index - 1)) }
            opener = nil
        } else if hasPrefix(line, fence, at: 0) {
            opener = index
        }
    }
    return assembleFencePass(lines, keep: keep, contentGroups: contentGroups)
}

private func assembleFencePass(_ lines: [Scalars], keep: [Bool], contentGroups: [ClosedRange<Int>]) -> FencePass {
    var removals: [Removal] = []
    var kept: Scalars = []
    var newStarts = [Int](repeating: -1, count: lines.count)
    var oldStart = 0
    for (index, line) in lines.enumerated() {
        if keep[index] {
            // Kotlin joins on `kept.isNotEmpty()`, so leading empty kept
            // lines contribute no newline; mirrored for wire parity.
            if !kept.isEmpty { kept.append("\n") }
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

/// Kotlin `CharSequence.trim()`: `Character.isWhitespace || isSpaceChar`.
private func trimmedKotlinStyle(_ line: Scalars) -> Scalars {
    guard let first = line.firstIndex(where: { !isKotlinWhitespace($0) }),
          let last = line.lastIndex(where: { !isKotlinWhitespace($0) })
    else { return [] }
    return Array(line[first...last])
}

private func isKotlinWhitespace(_ scalar: Unicode.Scalar) -> Bool {
    (0x09...0x0D).contains(scalar.value) || (0x1C...0x1F).contains(scalar.value) || isSeparator(scalar)
}

private func isSeparator(_ scalar: Unicode.Scalar) -> Bool {
    switch scalar.properties.generalCategory {
    case .spaceSeparator, .lineSeparator, .paragraphSeparator: return true
    default: return false
    }
}

// MARK: - Pass B: inline markers, single left-to-right scan

private struct InlinePass {
    let body: Scalars
    let spans: [MarkupSpan]
    let removals: [Removal]
}

/// One marker style. `flanked` content is `\S(?:[^excluded]*?\S)??` (the
/// bold/italic shape); otherwise it is `[^excluded]+?`. Both are matched
/// lazily so `**a** mid **b**` yields two spans instead of one greedy span
/// across the middle.
private struct InlineRule {
    let marker: Scalars
    let kind: MarkupSpan.Kind
    let excluded: Set<Unicode.Scalar>
    let flanked: Bool
}

/// Priority order at equal positions: code wins, then strike, bold, italic.
private let inlineRules: [InlineRule] = [
    InlineRule(marker: ["`"], kind: .code, excluded: ["`", "\n"], flanked: false),
    InlineRule(marker: ["~", "~"], kind: .strikethrough, excluded: ["\n"], flanked: false),
    InlineRule(marker: ["*", "*"], kind: .bold, excluded: ["\n"], flanked: true),
    InlineRule(marker: ["*"], kind: .italic, excluded: ["*", "\n"], flanked: true),
]

private struct InlineMatch {
    let rule: InlineRule
    /// Whole match including both markers.
    let range: ScalarRange
    var content: ScalarRange {
        ScalarRange(start: range.start + rule.marker.count, end: range.end - rule.marker.count)
    }
}

private func applyInlineMarkers(_ body: Scalars, protected: [ScalarRange]) -> InlinePass {
    var result: Scalars = []
    var spans: [MarkupSpan] = []
    var removals: [Removal] = []
    var searchFrom = 0
    while searchFrom < body.count, let match = earliestInlineMatch(body, from: searchFrom, protected: protected) {
        let marker = match.rule.marker.count
        let content = match.content
        result.append(contentsOf: body[searchFrom..<match.range.start])
        let spanStart = result.count
        result.append(contentsOf: body[content.start..<content.end])
        spans.append(ScalarRange(start: spanStart, end: result.count).span(match.rule.kind))
        removals.append(Removal(start: match.range.start, length: marker))
        removals.append(Removal(start: content.end, length: marker))
        searchFrom = match.range.end
    }
    result.append(contentsOf: body[Swift.min(searchFrom, body.count)...])
    return InlinePass(body: result, spans: spans, removals: removals)
}

/// Leftmost match, ties broken by rule priority. A match overlapping a code
/// block is skipped by retrying one scalar past its start.
private func earliestInlineMatch(_ body: Scalars, from: Int, protected: [ScalarRange]) -> InlineMatch? {
    var position = from
    while position < body.count {
        if let match = firstRuleMatch(body, at: position) {
            if !protected.contains(where: { $0.overlaps(match.range) }) { return match }
        }
        position += 1
    }
    return nil
}

private func firstRuleMatch(_ body: Scalars, at start: Int) -> InlineMatch? {
    for rule in inlineRules {
        if let end = matchEnd(rule, in: body, at: start) {
            return InlineMatch(rule: rule, range: ScalarRange(start: start, end: end))
        }
    }
    return nil
}

/// End (exclusive, after the closing marker) of `rule` matched at `start`.
private func matchEnd(_ rule: InlineRule, in body: Scalars, at start: Int) -> Int? {
    guard hasPrefix(body, rule.marker, at: start) else { return nil }
    let contentStart = start + rule.marker.count
    let contentEnd = rule.flanked
        ? flankedContentEnd(rule, in: body, from: contentStart)
        : plainContentEnd(rule, in: body, from: contentStart)
    return contentEnd.map { $0 + rule.marker.count }
}

/// Lazy `[^excluded]+?` followed by the closing marker.
private func plainContentEnd(_ rule: InlineRule, in body: Scalars, from contentStart: Int) -> Int? {
    var end = contentStart + 1
    while end <= body.count {
        if rule.excluded.contains(body[end - 1]) { return nil }
        if hasPrefix(body, rule.marker, at: end) { return end }
        end += 1
    }
    return nil
}

/// Lazy `\S(?:[^excluded]*?\S)??` followed by the closing marker: the
/// content starts and ends on non-space, and every scalar between is
/// outside `excluded`.
private func flankedContentEnd(_ rule: InlineRule, in body: Scalars, from contentStart: Int) -> Int? {
    guard contentStart < body.count, !isRegexSpace(body[contentStart]) else { return nil }
    var end = contentStart + 1
    while end <= body.count {
        if end >= contentStart + 3, rule.excluded.contains(body[end - 2]) { return nil }
        if !isRegexSpace(body[end - 1]), hasPrefix(body, rule.marker, at: end) { return end }
        end += 1
    }
    return nil
}

/// Regex `\s`. The Kotlin tests run on the JVM (ASCII `[ \t\n\x0B\f\r]`)
/// while Android devices use ICU (`[\t\n\f\r\p{Z}]`); the union keeps both
/// ASCII controls and Unicode separators from flanking a style.
private func isRegexSpace(_ scalar: Unicode.Scalar) -> Bool {
    scalar == " " || (0x09...0x0D).contains(scalar.value) || isSeparator(scalar)
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
