/// A token being completed. All selection and replacement offsets count
/// Unicode scalars, with an exclusive upper bound.
public struct ComposerSuggestionTrigger: Hashable, Sendable {
    public let kind: ComposerSuggestion.Kind
    public let query: String
    public let replacementRange: ComposerScalarRange

    /// A nil selection means a caret at the end of the draft. An active
    /// text selection does not trigger suggestions.
    public static func find(in text: String, selection: ComposerScalarRange? = nil) -> Self? {
        let scalars = Array(text.unicodeScalars)
        let caret = selection?.lowerBound ?? scalars.count
        guard (selection?.isEmpty ?? true), caret > 0, caret <= scalars.count else { return nil }

        let beforeCaret = scalars[..<caret]
        let start = (beforeCaret.lastIndex(where: isWebWhitespace) ?? -1) + 1
        if start < caret {
            let query = scalars[(start + 1)..<caret]
            let range = start..<caret
            if scalars[start] == "@" {
                return Self(kind: .mention, query: String(String.UnicodeScalarView(query)), replacementRange: range)
            }
            if scalars[start] == ":", query.count >= EmojiShortcodes.minimumQueryLength,
               query.allSatisfy(isEmojiKeywordScalar) {
                return Self(kind: .emoji, query: String(String.UnicodeScalarView(query)), replacementRange: range)
            }
        }

        // Slash submit is anchored to the leading command. Query only the
        // part before the caret, but replace the whole command word on pick.
        guard let command = SlashTrigger.parse(text) else { return nil }
        if caret <= command.wordRange.upperBound {
            let query = String(String.UnicodeScalarView(scalars[1..<caret]))
            return Self(kind: .slash, query: query, replacementRange: command.wordRange)
        }
        // A bare "/ " before a draft is also how the command button opens
        // the list. Do not arm it from a later line.
        guard command.prefix.isEmpty, !beforeCaret.contains(where: { $0 == "\n" || $0 == "\r" }) else { return nil }
        return Self(kind: .slash, query: "", replacementRange: command.wordRange)
    }

    private static func isEmojiKeywordScalar(_ scalar: Unicode.Scalar) -> Bool {
        switch scalar {
        case "a"..."z", "A"..."Z", "0"..."9", "_", "+", "-": true
        default: false
        }
    }

    // ECMAScript's \s, used by the web mention and emoji patterns.
    private static func isWebWhitespace(_ scalar: Unicode.Scalar) -> Bool {
        switch scalar.value {
        case 0x09...0x0D, 0x20, 0xA0, 0x1680, 0x2000...0x200A, 0x2028, 0x2029, 0x202F, 0x205F, 0x3000, 0xFEFF: true
        default: false
        }
    }
}
