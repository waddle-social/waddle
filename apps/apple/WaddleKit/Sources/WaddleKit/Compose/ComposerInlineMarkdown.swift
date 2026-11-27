// MARK: - Pass B: inline markers, single left-to-right scan

struct InlinePass {
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
private let markerScalars = Set(inlineRules.flatMap(\.marker))

private struct InlineMatch {
    let rule: InlineRule
    /// Whole match including both markers.
    let range: ScalarRange
    var content: ScalarRange {
        ScalarRange(start: range.start + rule.marker.count, end: range.end - rule.marker.count)
    }
}

func applyInlineMarkers(_ body: Scalars, protected: [ScalarRange], literal: [ScalarRange]) -> InlinePass {
    // Hide label punctuation from the matcher without changing offsets or the emitted text.
    var syntax = body
    for range in literal where range.start < body.count {
        for index in range.start..<min(range.end, body.count) where markerScalars.contains(body[index]) {
            syntax[index] = "\u{FFFC}"
        }
    }
    var result: Scalars = []
    var spans: [MarkupSpan] = []
    var removals: [Removal] = []
    var searchFrom = 0
    while searchFrom < body.count, let match = earliestInlineMatch(syntax, from: searchFrom, protected: protected) {
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
        if let match = firstRuleMatch(body, at: position), !protected.contains(where: { $0.overlaps(match.range) }) {
            return match
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

/// Checks code at a potential room trigger without composing the draft.
func inlineCodeOverlaps(_ range: Range<Int>, in line: Scalars) -> Bool {
    var position = 0
    while position < range.upperBound {
        guard let match = firstRuleMatch(line, at: position) else {
            position += 1
            continue
        }
        if match.rule.kind == .code, (match.content.start..<match.content.end).overlaps(range) {
            return true
        }
        position = match.range.end
    }
    return false
}
