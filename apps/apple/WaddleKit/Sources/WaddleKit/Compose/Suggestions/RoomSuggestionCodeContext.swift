/// Room links are literal body text and cannot be inserted in code or on a fence opener.
enum RoomSuggestionCodeContext {
    static func overlapsCode(_ range: ComposerScalarRange, in scalars: Scalars) -> Bool {
        let draft = ComposerDraftLines(scalars)
        let index = draft.line(at: range.lowerBound)
        if draft.codeBlocks.contains(where: { $0.opening <= index && index < $0.closing }) { return true }

        // A room trigger and any enclosing inline code stay on the same line.
        let line = draft.lines[index].beforeLF
        let start = line.lowerBound
        guard scalars[start..<range.lowerBound].contains("`"),
              scalars[range.upperBound..<line.upperBound].contains("`") else { return false }
        let local = (range.lowerBound - start)..<(range.upperBound - start)
        return inlineCodeOverlaps(local, in: Array(scalars[line]))
    }
}
