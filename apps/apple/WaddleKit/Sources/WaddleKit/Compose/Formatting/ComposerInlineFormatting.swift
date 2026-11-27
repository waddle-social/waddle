enum ComposerInlineFormatting {
    private typealias Style = (kind: MarkupSpan.Kind, marker: String)
    private static let styles: [ComposerFormat: Style] = [
        .bold: (.bold, "**"), .italic: (.italic, "*"), .strikethrough: (.strikethrough, "~~"), .code: (.code, "`"),
    ]

    static func toggle(
        _ format: ComposerFormat, in draft: ComposerDraftLines, selection: ComposerScalarRange, roomLinks: [RoomLinkDraft]
    ) -> ComposerTextEdit {
        guard let style = styles[format] else { return draft.edit([], selection: selection) }
        if selection.isEmpty {
            return editCaret(in: draft, at: selection.lowerBound, marker: style.marker, roomLinks: roomLinks)
        }
        let parsed = Projection(draft, roomLinks: roomLinks)
        guard let selected = parsed.selectedContent(selection),
              let edit = parsed.toggle(style, selection: selected)
        else { return draft.edit([], selection: selection) }
        return edit
    }

    static func active(
        _ selection: ComposerScalarRange, in draft: ComposerDraftLines, roomLinks: [RoomLinkDraft]
    ) -> Set<ComposerFormat> {
        let parsed = Projection(draft, roomLinks: roomLinks)
        guard let selected = parsed.selectedContent(selection) else { return [] }
        return Set(styles.filter { parsed.isStyled($0.value.kind, throughout: selected) }.keys)
    }

    private static func editCaret(
        in draft: ComposerDraftLines, at caret: Int, marker: String, roomLinks: [RoomLinkDraft]
    ) -> ComposerTextEdit {
        let pair = Array((marker + marker).unicodeScalars)
        let start = caret - pair.count / 2
        let end = start + pair.count
        if hasPrefix(draft.scalars, pair, at: start),
           (start == 0 || draft.scalars[start - 1] != pair[0]),
           (end == draft.scalars.count || draft.scalars[end] != pair[0]) {
            // The empty-pair command works on the editor's untrimmed text.
            let (_, fences, inline) = ComposerMarkdown.parse(draft, references: .init(roomLinks: roomLinks))
            let unfenced = fences.retaining(Array(draft.scalars.indices))
            if !inline.removals.contains(where: { $0.length == pair.count && unfenced[$0.start] == start }),
               !roomLinks.contains(where: { $0.range.overlaps(start..<end) }) {
                return draft.edit([ComposerDraftChange(start..<end, "")], selection: caret..<caret)
            }
        }
        return draft.wrapping(caret..<caret, opening: marker, closing: marker)
    }

    private struct Projection {
        let draft: ComposerDraftLines
        let roomLinks: [RoomLinkDraft]
        let message: ComposedMarkdown
        let fences: [Removal]
        let inline: InlinePass
        let unfenced: [Int]
        let visible: [Int]
        let trimmed: Range<Int>

        init(_ draft: ComposerDraftLines, roomLinks: [RoomLinkDraft]) {
            self.draft = draft
            self.roomLinks = roomLinks
            // Apply the same trim and reference shift as MessageComposer.
            // Keep the raw draft offsets for selection and edit relocation.
            let (text, start) = MessageComposer.trim(draft.text(in: draft.scalars.indices))
            let content = ComposerDraftLines(Array(text.unicodeScalars))
            trimmed = start..<(start + content.scalars.count)
            let references = DraftReferences(roomLinks: roomLinks).shifted(by: -start, within: trimmed.count)
            (message, fences, inline) = ComposerMarkdown.parse(content, references: references)
            unfenced = fences.retaining(Array(trimmed))
            // A final removed fence can leave a separator in the removal
            // map that was never emitted by the parser.
            visible = Array(inline.removals.retaining(unfenced).prefix(inline.body.count))
        }

        func sentRange(_ range: Range<Int>) -> Range<Int> {
            let clipped = range.clamped(to: trimmed)
            let relative = (clipped.lowerBound - trimmed.lowerBound)..<(clipped.upperBound - trimmed.lowerBound)
            let sent = rebase(relative, fences: fences, inline: inline.removals) ?? 0..<0
            return sent.clamped(to: 0..<visible.count)
        }

        func selectedContent(_ range: Range<Int>) -> Range<Int>? {
            ComposerScalars.contentRange(inline.body[sentRange(range)])
        }

        func isStyled(_ kind: MarkupSpan.Kind, throughout selection: Range<Int>) -> Bool {
            selection.allSatisfy { offset in
                inline.body[offset].properties.isWhitespace ||
                    inline.spans.contains { $0.kind == kind && $0.start <= offset && offset < $0.end }
            }
        }

        func toggle(_ style: Style, selection: Range<Int>) -> ComposerTextEdit? {
            let runs = inline.spans.enumerated().compactMap { index, span in
                span.kind == style.kind ? (index: index, range: span.start..<span.end) : nil
            }
            let affected: [(index: Int, range: Range<Int>)]
            let wanted: [Range<Int>]
            if isStyled(style.kind, throughout: selection) {
                affected = runs.filter { $0.range.overlaps(selection) }
                let needsFlankingText = style.kind == .bold || style.kind == .italic
                wanted = affected.flatMap { run in
                    let before = run.range.clamped(to: 0..<selection.lowerBound)
                    let after = run.range.clamped(to: selection.upperBound..<visible.count)
                    guard needsFlankingText else { return [before, after].filter { !$0.isEmpty } }
                    // Whitespace next to a cut stays outside a bold or italic run.
                    let left = ComposerScalars.contentRange(inline.body[before]).map { before.lowerBound..<$0.upperBound }
                    let right = ComposerScalars.contentRange(inline.body[after]).map { $0.lowerBound..<after.upperBound }
                    return [left, right].compactMap { $0 }
                }
            } else {
                // Merge runs that intersect or meet the newly styled text.
                affected = runs.filter {
                    $0.range.lowerBound <= selection.upperBound && selection.lowerBound <= $0.range.upperBound
                }
                let start = min(selection.lowerBound, affected.first?.range.lowerBound ?? selection.lowerBound)
                let end = max(selection.upperBound, affected.last?.range.upperBound ?? selection.upperBound)
                wanted = [start..<end]
            }
            let removedOffsets = Set(affected.flatMap { run in
                inline.removals[(2 * run.index)..<(2 * run.index + 2)].flatMap { removal in
                    unfenced[removal.start..<(removal.start + removal.length)]
                }
            })

            var insertions: [Int: String] = [:]
            for range in wanted {
                insertions[visible[range.lowerBound], default: ""] += style.marker
                insertions[visible[range.upperBound - 1] + 1, default: ""] += style.marker
            }
            let changes = removedOffsets.union(insertions.keys).map { offset in
                ComposerDraftChange(offset..<(offset + (removedOffsets.contains(offset) ? 1 : 0)), insertions[offset] ?? "")
            }
            let result = draft.edit(changes, selection: 0..<0)
            let movedLinks = RoomLinks.relocate(roomLinks, from: draft.text(in: draft.scalars.indices), applying: result.replacement)
            let next = Projection(ComposerDraftLines(Array(result.text.unicodeScalars)), roomLinks: movedLinks)

            // An inserted star must not silently replace an identical,
            // originally literal star.
            var pending = result.changes[...]
            var shift = 0
            let kept = visible.map { offset in
                while let change = pending.first, change.range.upperBound <= offset {
                    shift += change.delta
                    pending = pending.dropFirst()
                }
                return offset + shift
            }
            let selected = kept[selection.lowerBound]..<(kept[selection.upperBound - 1] + 1)
            let expected = (runs.filter { run in !affected.contains(where: { $0.index == run.index }) }.map(\.range) + wanted)
                .sorted { $0.lowerBound < $1.lowerBound }
            guard next.visible == kept, next.inline.body == inline.body, next.sentRange(selected) == selection,
                  movedLinks.count == roomLinks.count,
                  next.message.spans.filter({ $0.kind != style.kind }) == message.spans.filter({ $0.kind != style.kind }),
                  next.inline.spans.filter({ $0.kind == style.kind }).map({ $0.start..<$0.end }) == expected
            else { return nil }
            return ComposerTextEdit(text: result.text, selection: selected, changes: result.changes)
        }
    }
}

private extension [Removal] {
    func retaining(_ offsets: [Int]) -> [Int] {
        var kept: [Int] = []
        var start = 0
        for removal in self {
            kept.append(contentsOf: offsets[start..<removal.start])
            start = Swift.min(offsets.count, removal.start + removal.length)
        }
        kept.append(contentsOf: offsets[start...])
        return kept
    }
}
