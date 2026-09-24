import Foundation
import WaddleKit

/// What the body renderer needs from a row.
struct RichTextInput {
    /// `TimelineItem.body`: fallback stripped, corrections applied.
    let displayedBody: String
    /// `WireMessage.body`, which the span and reference offsets count over
    /// unless the row was corrected.
    let wireBody: String?
    /// The reply fallback the offsets include (the correction's, once
    /// corrected).
    let fallback: Range<Int>?
    /// A XEP-0308 correction replaced body, spans and references; the
    /// offsets then count over the correction's wire body.
    let isEdited: Bool
    let spans: [MarkupSpan]
    let references: [Reference]
    let ownNick: String
    /// Classifies a reference as a mention; nil for non-mentions.
    let mentionKind: (Reference) -> RichMentionKind?
    /// Leading displayed-body scalars not rendered: the XEP-0245 "/me "
    /// prefix of an action line. Offsets still count over the full body.
    let hiddenPrefix: Int
}

/// Turns a displayed body plus its XEP-0394 spans and XEP-0372 references
/// into blocks of styled runs. Pure; the view maps runs to attributes.
enum RichTextLayout {
    static func blocks(
        for input: RichTextInput,
        detectLinks: (String) -> [RichDetectedLink]
    ) -> [RichBlock] {
        let scalars = Array(input.displayedBody.unicodeScalars)
        let styled = styledRanges(for: input, detectLinks: detectLinks)
        let hidden = min(max(input.hiddenPrefix, 0), scalars.count)
        var blocks: [RichBlock] = []
        var cursor = hidden
        for block in nonOverlapping(styled.blocks) {
            guard let range = MeAction.visibleRange(block.range, hiding: hidden) else { continue }
            if range.lowerBound > cursor {
                blocks += paragraph(scalars, cursor..<range.lowerBound, inline: styled.inline, detectLinks: detectLinks)
            }
            blocks += render(BlockRange(kind: block.kind, range: range), scalars: scalars, inline: styled.inline, detectLinks: detectLinks)
            cursor = range.upperBound
        }
        if cursor < scalars.count {
            blocks += paragraph(scalars, cursor..<scalars.count, inline: styled.inline, detectLinks: detectLinks)
        }
        return blocks
    }

    // MARK: - Offsets

    private enum BlockKind {
        case code
        case quote
    }

    private struct BlockRange {
        let kind: BlockKind
        let range: Range<Int>
    }

    /// Spans and references rebased onto the displayed body. Offsets that
    /// cannot be rebased are dropped rather than styling the wrong text.
    private static func styledRanges(for input: RichTextInput, detectLinks: (String) -> [RichDetectedLink]) -> (inline: [RichStyledRange], blocks: [BlockRange]) {
        var inline: [RichStyledRange] = []
        var blocks: [BlockRange] = []
        if let mapping = mapping(for: input) {
            for span in input.spans {
                guard let range = mapping.displayedRange(ofWire: span.start, span.end) else { continue }
                switch span.kind {
                case .bold: inline.append(RichStyledRange(style: .bold, range: range))
                case .italic: inline.append(RichStyledRange(style: .italic, range: range))
                case .strikethrough: inline.append(RichStyledRange(style: .strikethrough, range: range))
                case .code: inline.append(RichStyledRange(style: .code, range: range))
                case let .link(url): inline.append(RichStyledRange(style: .link(url), range: range))
                case .codeBlock: blocks.append(BlockRange(kind: .code, range: range))
                case .blockquote: blocks.append(BlockRange(kind: .quote, range: range))
                }
            }
            for reference in input.references {
                guard let range = mapping.displayedRange(ofWire: reference.begin, reference.end) else { continue }
                if let kind = input.mentionKind(reference) {
                    inline.append(RichStyledRange(style: .mention(kind), range: range))
                } else if let url = webURL(reference) {
                    inline.append(RichStyledRange(style: .link(url), range: range))
                }
            }
        }
        // XEP-0372 references remain authoritative. This fallback only adds
        // presentation emphasis to an exact own-nick token in visible text.
        // It never changes mention notifications or unread state.
        let visibleScalars = Array(input.displayedBody.unicodeScalars)
        let visibleText = RichTextSegmenter.string(visibleScalars[...])
        let links = inline.compactMap { styled -> Range<Int>? in
            if case .link = styled.style { return styled.range }
            return nil
        } + detectLinks(visibleText).map(\.range)
        let referencedMentions = inline.compactMap { styled -> Range<Int>? in
            if case .mention = styled.style { return styled.range }
            return nil
        }
        let codeBlockRanges = blocks.compactMap { block -> Range<Int>? in
            if case .code = block.kind { return block.range }
            return nil
        }
        let codeRanges = inline.filter { $0.style == .code }.map(\.range) + codeBlockRanges
        for range in MentionTokens.ownNickRanges(input.ownNick, in: visibleText) {
            guard !codeRanges.contains(where: { $0.overlaps(range) }),
                  !links.contains(where: { $0.overlaps(range) }),
                  !referencedMentions.contains(where: { $0.overlaps(range) }) else { continue }
            inline.append(RichStyledRange(style: .mention(.me), range: range))
        }
        return (inline, blocks)
    }

    private static func mapping(for input: RichTextInput) -> WireBodyMapping? {
        if input.isEdited {
            return WireBodyMapping(correctedBody: input.displayedBody, fallback: input.fallback)
        }
        guard let wireBody = input.wireBody else { return nil }
        return WireBodyMapping(wireBody: wireBody, fallback: input.fallback, displayedBody: input.displayedBody)
    }

    /// A XEP-0372 `data` reference to a web page renders as a link.
    private static func webURL(_ reference: Reference) -> URL? {
        guard reference.kind == .data, let url = URL(string: reference.uri),
              let scheme = url.scheme?.lowercased(), scheme == "https" || scheme == "http"
        else { return nil }
        return url
    }

    /// Earliest block wins where two overlap.
    private static func nonOverlapping(_ blocks: [BlockRange]) -> [BlockRange] {
        var kept: [BlockRange] = []
        for block in blocks.sorted(by: { ($0.range.lowerBound, $0.range.upperBound) < ($1.range.lowerBound, $1.range.upperBound) }) {
            if let last = kept.last, last.range.upperBound > block.range.lowerBound { continue }
            kept.append(block)
        }
        return kept
    }

    // MARK: - Blocks

    private static func render(
        _ block: BlockRange,
        scalars: [Unicode.Scalar],
        inline: [RichStyledRange],
        detectLinks: (String) -> [RichDetectedLink]
    ) -> [RichBlock] {
        switch block.kind {
        case .code:
            let range = trimmedNewlines(scalars, block.range)
            guard !range.isEmpty else { return [] }
            return [.code(RichTextSegmenter.string(scalars[range]))]
        case .quote:
            let range = trimmedNewlines(scalars, block.range)
            guard !range.isEmpty else { return [] }
            let local = localStyles(inline, in: range)
            let stripped = QuoteMarkers.strip(Array(scalars[range]), styles: local)
            let segments = styledSegments(stripped.scalars, styles: stripped.styles, detectLinks: detectLinks)
            return segments.isEmpty ? [] : [.quote(segments)]
        }
    }

    private static func paragraph(
        _ scalars: [Unicode.Scalar],
        _ range: Range<Int>,
        inline: [RichStyledRange],
        detectLinks: (String) -> [RichDetectedLink]
    ) -> [RichBlock] {
        let trimmed = trimmedNewlines(scalars, range)
        guard !trimmed.isEmpty else { return [] }
        let segments = styledSegments(Array(scalars[trimmed]), styles: localStyles(inline, in: trimmed), detectLinks: detectLinks)
        return segments.isEmpty ? [] : [.paragraph(segments)]
    }

    /// Adds detected links outside code and existing links, then segments.
    private static func styledSegments(
        _ scalars: [Unicode.Scalar],
        styles: [RichStyledRange],
        detectLinks: (String) -> [RichDetectedLink]
    ) -> [RichSegment] {
        let text = RichTextSegmenter.string(scalars[...])
        let blocked = styles.filter { styled in
            switch styled.style {
            case .code, .link: return true
            default: return false
            }
        }
        let detected = detectLinks(text).filter { link in
            !link.range.isEmpty
                && link.range.upperBound <= scalars.count
                && !blocked.contains { $0.range.overlaps(link.range) }
        }
        let all = styles + detected.map { RichStyledRange(style: .link($0.url), range: $0.range) }
        return RichTextSegmenter.segments(scalars, styles: all)
    }

    /// Styles intersected with `range`, rebased to its start.
    private static func localStyles(_ styles: [RichStyledRange], in range: Range<Int>) -> [RichStyledRange] {
        styles.compactMap { styled in
            let lower = max(styled.range.lowerBound, range.lowerBound)
            let upper = min(styled.range.upperBound, range.upperBound)
            guard lower < upper else { return nil }
            return RichStyledRange(style: styled.style, range: (lower - range.lowerBound)..<(upper - range.lowerBound))
        }
    }

    private static func trimmedNewlines(_ scalars: [Unicode.Scalar], _ range: Range<Int>) -> Range<Int> {
        var lower = range.lowerBound
        var upper = range.upperBound
        while lower < upper, isNewline(scalars[lower]) { lower += 1 }
        while upper > lower, isNewline(scalars[upper - 1]) { upper -= 1 }
        return lower..<upper
    }

    private static func isNewline(_ scalar: Unicode.Scalar) -> Bool {
        scalar == "\n" || scalar == "\r"
    }
}

/// Removes the `>` marker (and one following space) that starts each line
/// of a XEP-0394 blockquote, which senders keep in the body.
enum QuoteMarkers {
    static func strip(_ scalars: [Unicode.Scalar], styles: [RichStyledRange]) -> (scalars: [Unicode.Scalar], styles: [RichStyledRange]) {
        var removed = Set<Int>()
        var lineStart = true
        var index = 0
        while index < scalars.count {
            let scalar = scalars[index]
            if lineStart, scalar == ">" {
                removed.insert(index)
                if index + 1 < scalars.count, scalars[index + 1] == " " {
                    removed.insert(index + 1)
                    index += 1
                }
                lineStart = false
            } else {
                lineStart = scalar == "\n"
            }
            index += 1
        }
        // newOffset[i]: kept scalars before old offset i.
        var newOffset = [Int](repeating: 0, count: scalars.count + 1)
        var kept: [Unicode.Scalar] = []
        for (offset, scalar) in scalars.enumerated() {
            newOffset[offset] = kept.count
            if !removed.contains(offset) { kept.append(scalar) }
        }
        newOffset[scalars.count] = kept.count
        let remapped = styles.compactMap { styled -> RichStyledRange? in
            let lower = newOffset[min(max(styled.range.lowerBound, 0), scalars.count)]
            let upper = newOffset[min(max(styled.range.upperBound, 0), scalars.count)]
            return lower < upper ? RichStyledRange(style: styled.style, range: lower..<upper) : nil
        }
        return (kept, remapped)
    }
}
