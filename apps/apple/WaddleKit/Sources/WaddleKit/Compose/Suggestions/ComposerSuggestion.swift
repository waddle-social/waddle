/// A completion row and its plain-text edit.
public enum ComposerSuggestion: Hashable, Sendable {
    public enum Kind: Hashable, Sendable {
        case mention
        case emoji
        case slash
    }

    case mention(MentionCandidate)
    case emoji(EmojiShortcode)
    case slash(SlashCandidate)

    static let maximumRows = 8

    public var kind: Kind { completion.kind }

    public func accept(in text: String, replacing range: ComposerScalarRange) -> ComposerTextEdit {
        let completion = completion
        var range = range
        // A command consumes one existing space before its argument.
        if completion.kind == .slash, text.unicodeScalars.dropFirst(range.upperBound).first == " " {
            range = range.lowerBound..<(range.upperBound + 1)
        }
        return ComposerInsertion.replacingSelection(with: completion.text + " ", in: text, selection: range)
    }

    private var completion: (kind: Kind, text: String) {
        switch self {
        case let .mention(candidate): (.mention, candidate.token)
        case let .emoji(candidate): (.emoji, candidate.glyph)
        case let .slash(candidate): (.slash, "/" + candidate.name)
        }
    }

    public var recordedMention: RecordedMention? {
        guard case let .mention(candidate) = self else { return nil }
        return RecordedMention(token: candidate.token, target: candidate.target)
    }
}
