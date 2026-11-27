enum ComposerBlockFormatting {
    static func quoteMarker(in draft: ComposerDraftLines, at index: ComposerLineIndex) -> ComposerScalarRange? {
        let line = draft.lines[index].content
        guard draft.scalars[line].first == ">" else { return nil }
        let next = line.lowerBound + 1
        let end = next < line.upperBound && draft.scalars[next] == " " ? next + 1 : next
        return line.lowerBound..<end
    }

    static func toggleQuote(in draft: ComposerDraftLines, selection: ComposerScalarRange) -> ComposerTextEdit {
        let touched = draft.touched(by: selection)
        let markers = touched.compactMap { quoteMarker(in: draft, at: $0) }
        let changes = markers.count == touched.count
            ? markers.map { ComposerDraftChange($0, "") }
            : touched.map { index in
                let start = draft.lines[index].content.lowerBound
                return ComposerDraftChange(start..<start, "> ")
            }
        return draft.edit(changes, selection: selection)
    }

    struct Fence {
        let whole: ComposerScalarRange
        let inner: ComposerScalarRange
    }

    static func exactFence(in draft: ComposerDraftLines, selection: ComposerScalarRange) -> Fence? {
        for block in draft.codeBlocks {
            let opening = draft.lines[block.opening]
            let closing = draft.lines[block.closing]
            let first = opening.ending.upperBound
            let last = max(first, draft.lines[block.closing - 1].content.upperBound)
            let inner = first..<last
            let whole = opening.content.lowerBound..<closing.content.upperBound
            let includingClosingNewline = whole.lowerBound..<closing.ending.upperBound
            if selection == inner || selection == whole || selection == includingClosingNewline {
                // Wrapping before an existing CRLF inserts an LF to put the
                // closing fence on its own line. Remove that LF beside the CRLF.
                let following = block.closing + 1
                let separatesCRLF = opening.ending.count == 1 && closing.ending.count == 1
                    && draft.lines.indices.contains(following)
                    && draft.lines[following].content.isEmpty && draft.lines[following].ending.count == 2
                return Fence(whole: separatesCRLF ? includingClosingNewline : whole, inner: inner)
            }
        }
        return nil
    }

    static func toggleFence(in draft: ComposerDraftLines, selection: ComposerScalarRange) -> ComposerTextEdit {
        if let fence = exactFence(in: draft, selection: selection) {
            return draft.edit([
                ComposerDraftChange(fence.whole.lowerBound..<fence.inner.lowerBound, ""),
                ComposerDraftChange(fence.inner.upperBound..<fence.whole.upperBound, ""),
            ], selection: fence.inner)
        }

        let start = selection.lowerBound
        let end = selection.upperBound
        let startsLine = start == 0 || draft.scalars[start - 1] == "\n"
        let endsLine = end == draft.scalars.count || draft.scalars[end] == "\n"
        let opening = (startsLine ? "" : "\n") + "```\n"
        let closing = "\n```" + (endsLine ? "" : "\n")
        return draft.wrapping(selection, opening: opening, closing: closing)
    }
}
