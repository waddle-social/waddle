/// Toggles the draft markers that `ComposerMarkdown` understands. A nil
/// selection applies the style to the draft's trimmed content.
public enum ComposerFormatting {
    public static func apply(
        _ format: ComposerFormat, to text: String, selection: ComposerScalarRange?, roomLinks: [RoomLinkDraft] = []
    ) -> ComposerTextEdit {
        let scalars = Array(text.unicodeScalars)
        let range = ComposerScalars.clamp(selection ?? ComposerScalars.contentRange(scalars[...]), count: scalars.count)
        switch format {
        case .orderedList: return ComposerLists.toggle(.ordered, in: scalars, selection: range)
        case .bulletList: return ComposerLists.toggle(.bullet, in: scalars, selection: range)
        case .bold, .italic, .strikethrough, .code:
            return ComposerInlineFormatting.toggle(format, in: ComposerDraftLines(scalars), selection: range, roomLinks: roomLinks)
        case .codeBlock: return ComposerBlockFormatting.toggleFence(in: ComposerDraftLines(scalars), selection: range)
        case .quote: return ComposerBlockFormatting.toggleQuote(in: ComposerDraftLines(scalars), selection: range)
        }
    }
}

extension ComposerDraftLines {
    /// Keep the original text selected and record marker insertions separately
    /// so references can be relocated through the exact edits.
    func wrapping(_ selection: ComposerScalarRange, opening: String, closing: String) -> ComposerTextEdit {
        let lower = selection.lowerBound
        let upper = selection.upperBound
        let changes = selection.isEmpty
            ? [ComposerDraftChange(selection, opening + closing)]
            : [ComposerDraftChange(lower..<lower, opening), ComposerDraftChange(upper..<upper, closing)]
        let edit = edit(changes, selection: selection)
        let shift = opening.unicodeScalars.count
        return ComposerTextEdit(text: edit.text, selection: (lower + shift)..<(upper + shift), changes: edit.changes)
    }
}
