/// A completion row and its plain-text edit.
public enum ComposerSuggestion: Hashable, Sendable {
    public enum Kind: Hashable, Sendable {
        case mention
        case room
        case emoji
        case slash
    }

    case mention(MentionCandidate)
    case room(RoomLinks.Candidate)
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
        case let .room(candidate): (.room, candidate.label)
        case let .emoji(candidate): (.emoji, candidate.glyph)
        case let .slash(candidate): (.slash, "/" + candidate.name)
        }
    }

    public var recordedMention: RecordedMention? {
        guard case let .mention(candidate) = self else { return nil }
        return RecordedMention(token: candidate.token, target: candidate.target)
    }

    func roomLink(at start: Int) -> RoomLinkDraft? {
        guard case let .room(candidate) = self else { return nil }
        return RoomLinkDraft(room: candidate.room, range: start..<(start + candidate.label.unicodeScalars.count))
    }

    func matchesSelection(_ previous: Self) -> Bool {
        if case let .room(candidate) = self, case let .room(prior) = previous {
            return candidate.room == prior.room && candidate.label == prior.label
        }
        return self == previous
    }
}

extension RoomLinks.Candidate {
    public var addressDetail: String? { showsAddress ? room.description : nil }

    public var accessibilityName: String {
        addressDetail.map { "\(label), \($0)" } ?? label
    }
}

extension [RoomLinks.Candidate] {
    func disambiguatingRoomLabels() -> Self {
        let counts = Dictionary(grouping: self, by: \.label)
        return map { candidate in
            var candidate = candidate
            candidate.showsAddress = (counts[candidate.label]?.count ?? 0) > 1
            return candidate
        }
    }
}
