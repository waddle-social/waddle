import Foundation

/// Turns a displayed body plus its XEP-0394 spans and XEP-0372 references
/// into blocks of styled runs. Pure; the view maps runs to attributes.
public enum RichTextLayout {
    public static func blocks(
        for item: TimelineItem,
        account: AccountIdentity,
        detectLinks: (String) -> [RichDetectedLink] = { _ in [] }
    ) -> [RichBlock] {
        guard item.tombstone == nil else { return [] }
        let scalars = Array(item.body.unicodeScalars)
        let styled = styledRanges(for: item, account: account, detectLinks: detectLinks)
        let hidden = MeAction.hiddenScalarCount(ofBody: item.body)
        var blocks: [RichBlock] = []
        var cursor = hidden
        for block in styled.blocks {
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
    private static func styledRanges(
        for item: TimelineItem, account: AccountIdentity, detectLinks: (String) -> [RichDetectedLink]
    ) -> (inline: [RichStyledRange], blocks: [BlockRange]) {
        var inline: [RichStyledRange] = []
        var blocks: [BlockRange] = []
        if let mapping = WireBodyMapping(item: item) {
            for span in item.message.markupSpans {
                guard let range = mapping.displayedRange(ofWire: span.start, span.end) else { continue }
                switch span.kind {
                case .bold: inline.append(RichStyledRange(style: .bold, range: range))
                case .italic: inline.append(RichStyledRange(style: .italic, range: range))
                case .strikethrough: inline.append(RichStyledRange(style: .strikethrough, range: range))
                case .code: inline.append(RichStyledRange(style: .code, range: range))
                case let .link(url):
                    if !MessageRoomLinks.isPrivateURL(url) {
                        inline.append(RichStyledRange(style: .link(url), range: range))
                    }
                case .codeBlock: blocks.append(BlockRange(kind: .code, range: range))
                case .blockquote: blocks.append(BlockRange(kind: .quote, range: range))
                }
            }
            blocks = nonOverlapping(blocks)
            let references = item.message.references.compactMap { reference in
                mapping.displayedRange(ofWire: reference.begin, reference.end).map { (reference, $0) }
            }
            for (reference, range) in references {
                if let kind = MentionHighlight.kind(of: reference, account: account, in: item.conversation) {
                    inline.append(RichStyledRange(style: .mention(kind), range: range))
                } else if let url = webURL(reference) {
                    inline.append(RichStyledRange(style: .link(url), range: range))
                }
            }
            // `@everyone` / `@here` address the signed-in account too.
            for broadcast in item.message.broadcastSpans {
                guard let range = mapping.displayedRange(ofWire: broadcast.start, broadcast.end) else { continue }
                inline.append(RichStyledRange(style: .mention(.me), range: range))
            }
            appendRoomLinks(references, wireLength: mapping.wireLength, blocks: blocks, to: &inline)
        }
        // XEP-0372 references remain authoritative. This fallback only adds
        // presentation emphasis to an exact own-nick token in visible text.
        // It never changes mention notifications or unread state.
        let visibleText = item.body
        let links = inline.filter { $0.style.isLink }.map(\.range) + detectLinks(visibleText).map(\.range)
        let referencedMentions = inline.compactMap { styled -> Range<Int>? in
            if case .mention = styled.style { return styled.range }
            return nil
        }
        let codeBlockRanges = blocks.compactMap { block -> Range<Int>? in
            if case .code = block.kind { return block.range }
            return nil
        }
        let codeRanges = inline.filter { $0.style == .code }.map(\.range) + codeBlockRanges
        for range in MentionTokens.ownNickRanges(account.nick, in: visibleText) {
            guard !codeRanges.contains(where: { $0.overlaps(range) }),
                  !links.contains(where: { $0.overlaps(range) }),
                  !referencedMentions.contains(where: { $0.overlaps(range) }) else { continue }
            inline.append(RichStyledRange(style: .mention(.me), range: range))
        }
        return (inline, blocks)
    }

    /// A room reference must describe one tappable range. Code, mentions
    /// and links rendered over that range take priority.
    private static func appendRoomLinks(
        _ references: [(Reference, Range<Int>)], wireLength: Int, blocks: [BlockRange], to inline: inout [RichStyledRange]
    ) {
        var occupied = blocks.filter { $0.kind == .code }.map(\.range) + inline.compactMap { styled in
            switch styled.style {
            case .code, .mention, .link: styled.range
            default: nil
            }
        }
        for (reference, range) in references {
            guard reference.end <= wireLength, let room = RoomLinks.room(from: reference),
                  !occupied.contains(where: { $0.overlaps(range) }) else { continue }
            inline.append(RichStyledRange(style: .roomLink(room), range: range))
            occupied.append(range)
        }
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
        let blocked = styles.filter { $0.style == .code || $0.style.isLink }
        let detected = detectLinks(text).filter { link in
            !MessageRoomLinks.isPrivateURL(link.url)
                && !link.range.isEmpty
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
