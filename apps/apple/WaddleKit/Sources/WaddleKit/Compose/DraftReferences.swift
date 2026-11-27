/// Who a mention addresses. A user is sent as a XEP-0372 `xmpp:<barejid>`
/// reference; `@everyone` / `@here` are XEP-0513 `#channel` mentions the
/// Rust send builder derives from the typed token, so they have no URI.
public enum MentionTarget: Hashable, Sendable {
    case user(BareJID)
    case everyone
    case here
}

/// A mention the composer inserted: its target and its range over the raw
/// draft, in Unicode scalars (code points), end exclusive.
public struct MentionDraft: Hashable, Sendable {
    public let target: MentionTarget
    public var range: Range<Int>

    public init(target: MentionTarget, range: Range<Int>) {
        self.target = target
        self.range = range
    }
}

/// The two kinds of draft reference share storage across the editor and
/// send path. Mentions are tokens while editing and scalar ranges on send.
public struct ComposerReferences<Mention: Hashable & Sendable>: Hashable, Sendable {
    public var mentions: [Mention]
    public var roomLinks: [RoomLinkDraft]

    public init(mentions: [Mention] = [], roomLinks: [RoomLinkDraft] = []) {
        self.mentions = mentions
        self.roomLinks = roomLinks
    }
}

public typealias DraftReferences = ComposerReferences<MentionDraft>
public typealias RecordedDraftReferences = ComposerReferences<RecordedMention>

public enum RecordedDraftReference: Hashable, Sendable {
    case mention(RecordedMention)
    case roomLink(RoomLinkDraft)
}

extension RecordedDraftReferences {
    public mutating func append(_ reference: RecordedDraftReference) {
        switch reference {
        case let .mention(mention): mentions.append(mention)
        case let .roomLink(link): roomLinks.append(link)
        }
    }

    public mutating func relocate(from text: String, applying replacement: ComposerTextReplacement) {
        roomLinks = RoomLinks.relocate(roomLinks, from: text, applying: replacement)
    }

    public func located(in text: String) -> DraftReferences {
        DraftReferences(mentions: MentionTokens.locate(mentions, in: text), roomLinks: roomLinks)
    }
}

extension DraftReferences {
    func shifted(by delta: Int, within length: Int) -> Self {
        func shift(_ range: Range<Int>, clipping: Bool) -> Range<Int>? {
            let lower = range.lowerBound + delta
            let upper = range.upperBound + delta
            let start = max(0, lower)
            let end = min(length, upper)
            guard start < end, clipping || (start == lower && end == upper) else { return nil }
            return start..<end
        }

        return Self(
            mentions: mentions.compactMap { mention in
                shift(mention.range, clipping: true).map { MentionDraft(target: mention.target, range: $0) }
            },
            roomLinks: roomLinks.compactMap { link in
                shift(link.range, clipping: false).map { RoomLinkDraft(room: link.room, range: $0) }
            }
        )
    }

    func wireReferences(offset: Int) -> [Reference] {
        // Rust derives broadcast mentions from their typed token.
        let mentions = mentions.compactMap { mention -> Reference? in
            guard case let .user(jid) = mention.target else { return nil }
            return Reference(kind: .mention, uri: "xmpp:\(jid)", begin: mention.range.lowerBound + offset, end: mention.range.upperBound + offset)
        }
        let links = roomLinks.compactMap { link -> Reference? in
            guard let uri = RoomLinks.joinURI(for: link.room) else { return nil }
            return Reference(kind: .data, uri: uri, begin: link.range.lowerBound + offset, end: link.range.upperBound + offset)
        }
        return (links + mentions).sorted { $0.begin < $1.begin }
    }
}
