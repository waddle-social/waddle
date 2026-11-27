/// Plain Return's block rules, shared by both keyboards.
public enum ComposerReturn: Equatable, Sendable {
    case useDefault
    case edit(ComposerTextEdit)

    public static func decide(text: String, selection: ComposerScalarRange?) -> Self {
        let draft = ComposerDraftLines(Array(text.unicodeScalars))
        let selected = ComposerScalars.clamp(selection, count: draft.scalars.count)
        let first = draft.line(at: selected.lowerBound)
        let last = draft.line(at: selected.upperBound)
        // The native caret may lie between CR and LF.
        let start = min(selected.lowerBound, draft.lines[first].content.upperBound)
        let end = min(selected.upperBound, draft.lines[last].content.upperBound)
        let range = start..<end
        if let edit = codeEdit(in: draft, range: range, lines: first...last) { return .edit(edit) }
        let lists = ComposerListLines(draft)
        if let item = lists.lines[first].item {
            return .edit(splitList(lists, item: item, range: range, lines: first...last))
        }
        if let item = lists.lines[last].item {
            let replaced = range.lowerBound..<max(range.upperBound, item.marker.upperBound)
            let changes = lists.renumber(after: last)
            return .edit(replace(replaced, with: draft.newline(at: first), in: draft, followedBy: changes))
        }
        return .useDefault
    }

    private static func splitList(
        _ lists: ComposerListLines, item: ComposerListLines.Item, range: ComposerScalarRange,
        lines: ClosedRange<ComposerLineIndex>
    ) -> ComposerTextEdit {
        let draft = lists.draft
        let first = lines.lowerBound
        let last = lines.upperBound
        let markerEnd = item.marker.upperBound
        if range.isEmpty, markerEnd == draft.lines[first].content.upperBound {
            return replace(item.marker, with: "", in: draft, followedBy: lists.renumber(after: first))
        }
        let nextNumber = item.number.map(ComposerListLines.nextNumber(after:))
        let marker = nextNumber.map(ComposerListLines.Kind.ordered.marker)
            ?? draft.text(in: item.marker)
        let start = max(range.lowerBound, markerEnd)
        let end = max(range.upperBound, lists.lines[last].item?.marker.upperBound ?? 0)
        let prefix = draft.text(in: lists.prefix(at: first)) + marker
        let renumber = if let nextNumber, (first..<last).allSatisfy({ lists.continues(after: $0) }) {
            lists.renumber(after: last, startingAt: ComposerListLines.nextNumber(after: nextNumber))
        } else {
            [ComposerDraftChange]()
        }
        return replace(start..<end, with: draft.newline(at: first) + prefix, in: draft, followedBy: renumber)
    }

    private static func codeEdit(
        in draft: ComposerDraftLines, range: ComposerScalarRange, lines: ClosedRange<ComposerLineIndex>
    ) -> ComposerTextEdit? {
        let first = lines.lowerBound
        let last = lines.upperBound
        let startCode = draft.codeBlocks.first { $0.contentLines.contains(first) }
        let endCode = draft.codeBlocks.first { $0.contentLines.contains(last) }
        let newline = draft.newline(at: first)
        if let startCode {
            if last <= startCode.closing {
                let end = last == startCode.closing ? draft.lines[last - 1].content.upperBound : range.upperBound
                return insideCode(startCode, in: draft, line: first, range: range.lowerBound..<end)
            }
            let split = newline + "```" + newline + "```" + newline
            if endCode != nil { return replace(range, with: split, in: draft) }
            return splitCodeIntoPlain(draft, range: range, endLine: last, split: split)
        }
        guard let endCode else { return nil }
        let closing = draft.lines[endCode.closing - 1].ending.lowerBound..<draft.lines[endCode.closing].content.upperBound
        return replace(range, with: newline, in: draft, followedBy: [ComposerDraftChange(closing, "")])
    }

    private static func insideCode(
        _ code: ComposerDraftLines.CodeBlock, in draft: ComposerDraftLines, line: ComposerLineIndex,
        range: ComposerScalarRange
    ) -> ComposerTextEdit {
        if range.isEmpty, draft.lines[line].content.isEmpty, line == code.closing - 1, line > code.opening + 1 {
            // Drop the final blank code line and continue after the closing fence.
            let after = draft.lines[code.closing].content.upperBound
            return draft.edit([
                ComposerDraftChange(draft.lines[line - 1].ending, ""),
                ComposerDraftChange(after..<after, draft.newline(at: code.closing)),
            ], selection: after..<after)
        }
        return replace(range, with: draft.newline(at: line), in: draft)
    }

    /// The surviving plain text becomes a new, fenced block.
    private static func splitCodeIntoPlain(
        _ draft: ComposerDraftLines, range: ComposerScalarRange, endLine: ComposerLineIndex, split: String
    ) -> ComposerTextEdit {
        let end = draft.lines[endLine].content.upperBound
        let suffix = draft.text(in: range.upperBound..<end)
        let newline = draft.newline(at: draft.line(at: range.lowerBound))
        let edit = replace(range.lowerBound..<end, with: split + suffix + newline + "```", in: draft)
        let caret = range.lowerBound + split.unicodeScalars.count
        return ComposerTextEdit(text: edit.text, selection: caret..<caret, changes: edit.changes)
    }

    private static func replace(
        _ range: ComposerScalarRange, with text: String, in draft: ComposerDraftLines,
        followedBy changes: [ComposerDraftChange] = []
    ) -> ComposerTextEdit {
        let end = range.upperBound
        return draft.edit([ComposerDraftChange(range, text)] + changes, selection: end..<end)
    }
}
