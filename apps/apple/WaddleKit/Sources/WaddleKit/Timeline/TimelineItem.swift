import Foundation

/// One emoji's aggregated XEP-0444 state on a row.
public struct ReactionGroup: Hashable, Sendable, Identifiable {
    public var id: String { emoji }
    public let emoji: String
    public let count: Int
    /// The signed-in account is among the reactors (tap toggles it off).
    public let includesMine: Bool
    /// Display names of the reactors, first-reacted order.
    public let reactors: [String]

    public init(emoji: String, count: Int, includesMine: Bool, reactors: [String]) {
        self.emoji = emoji
        self.count = count
        self.includesMine = includesMine
        self.reactors = reactors
    }
}

/// Why a row's content is gone.
public enum Tombstone: Hashable, Sendable {
    /// XEP-0424: retracted by its author.
    case retracted
    /// XEP-0425: removed by a room moderator.
    case moderated(by: String?, reason: String?)
}

/// A rendered timeline row.
public struct TimelineItem: Hashable, Sendable, Identifiable {
    /// Dedupe identity: stanza id, else origin id, else message id.
    public let id: String
    /// Stable local row key for rendering, scrolling and highlighting.
    /// Distinct messages may carry identical sender-chosen wire ids.
    public let presentationID: String
    public let conversation: ConversationID
    public let isMine: Bool
    /// The parsed stanza backing the row.
    public var message: WireMessage
    /// Body with the XEP-0428 reply fallback removed and any XEP-0308
    /// correction applied.
    public var body: String
    public var isEdited: Bool
    public var tombstone: Tombstone?
    public var reactions: [ReactionGroup]
    /// Scores the room fastened to this message (XEP-0422,
    /// `urn:waddle:safety-scores:1`), visible to every participant. Nil
    /// until judged, after a clear, and on removed messages.
    public var safetyScores: SafetyScores?
    /// The call this row announced is over, as the room said (XEP-0422,
    /// `urn:waddle:call-thread:0`).
    public var callEnded: CallThreadEnded?
    /// Local send not yet reflected or archived.
    public var isLocalEcho: Bool
    /// When this client first saw the row. Live stanzas carry no timestamp,
    /// so this is their display time.
    public let receivedAt: Date

    public init(
        id: String,
        presentationID: String = UUID().uuidString,
        conversation: ConversationID,
        isMine: Bool,
        message: WireMessage,
        body: String,
        isEdited: Bool = false,
        tombstone: Tombstone? = nil,
        reactions: [ReactionGroup] = [],
        safetyScores: SafetyScores? = nil,
        isLocalEcho: Bool = false,
        receivedAt: Date = Date()
    ) {
        self.id = id
        self.presentationID = presentationID
        self.conversation = conversation
        self.isMine = isMine
        self.message = message
        self.body = body
        self.isEdited = isEdited
        self.tombstone = tombstone
        self.reactions = reactions
        self.safetyScores = safetyScores
        self.isLocalEcho = isLocalEcho
        self.receivedAt = receivedAt
    }

    /// Display time: the wire timestamp, else first sight.
    public var sentAt: Date { message.timestamp ?? receivedAt }

    public var identity: MessageIdentity { message.identity }
    public var from: JID? { message.from }
    public var timestamp: Date? { message.timestamp }

    /// Display name of the author: the occupant nick in a room, the
    /// localpart (or domain) of the peer in 1:1.
    public var authorName: String {
        guard let from = message.from else { return "" }
        if conversation.isRoom {
            return from.resource ?? from.bare.localpart ?? from.bare.domain
        }
        return from.bare.localpart ?? from.bare.domain
    }

    /// The real bare JID behind the author: the sender in 1:1, only the
    /// stamped real JID in a room (the archived `muc#user` item, our local
    /// echo, or the occupant when an undelayed message arrived), since a
    /// nick may have changed hands since.
    public var authorJID: BareJID? {
        guard conversation.isRoom else { return message.from?.bare }
        return message.authorRealJID
    }

    /// Display-only grouping key: occupant JID plus its captured real JID
    /// in rooms, bare sender JID in 1:1. A nick handover starts a new group;
    /// unstamped rows keep their occupant grouping without guessing identity.
    public var authorKey: String {
        guard let from = message.from else { return "" }
        guard conversation.isRoom else { return from.bare.description }
        if let author = message.authorRealJID {
            return "\(from.description)\u{0}\(author.description)"
        }
        return from.description
    }

    /// Thread replies render only inside their thread. A call's
    /// announcement is the head of its thread, the call's chat, though the
    /// thread has an id of its own and not the announcement's.
    public var isFeedVisible: Bool {
        guard let thread = message.thread else { return true }
        return identity.all.contains(thread) || callAnchor != nil
    }

    /// Whether the row is one of the messages an unread count counts:
    /// someone else's, still there, and a message. A room's announcement of
    /// a call is a row and not a message: the server's count leaves it out,
    /// and every place that counts rows must agree with it and with each other.
    public var countsAsUnread: Bool {
        !isMine && tombstone == nil && callAnchor == nil
    }

    /// The call this row announces, when the room itself posted it: the
    /// server writes the announcement from the room's bare JID, and an
    /// occupant who marks a message of their own as one is not believed.
    public var callAnchor: CallThreadAnchor? {
        guard let anchor = message.callThread, anchor.kind == .room, conversation.isRoom, message.isGroupchat,
              let from = message.from, from.resource == nil, from.bare == conversation.jid else { return nil }
        return anchor
    }

    /// The XEP-0359 stanza id the room assigned (`by` is the room). In a
    /// room this is the only id other occupants cannot forge.
    public var roomStanzaID: String? {
        conversation.isRoom ? identity.stanzaID(assignedBy: conversation.jid) : nil
    }

    /// The id the room's archive gave this row (XEP-0313), when the row
    /// came from that archive. Room-assigned, never a sender-chosen alias.
    var roomArchiveUID: String? {
        guard conversation.isRoom, case let .archive(uid) = message.source, !uid.isEmpty else { return nil }
        return uid
    }

    /// The row in `items` with the same room-assigned stanza id.
    func roomStanzaMatch<Items: Collection>(in items: Items) -> Items.Index? where Items.Element == TimelineItem {
        guard let roomID = roomStanzaID else { return nil }
        return items.firstIndex { $0.conversation == conversation && $0.roomStanzaID == roomID }
    }

    /// The single row in `items` from the same room archive entry, unless
    /// the two carry different room-assigned stanza ids.
    func roomArchiveMatch(in items: [TimelineItem]) -> Int? {
        guard let uid = roomArchiveUID else { return nil }
        let matches = items.indices.filter {
            items[$0].conversation == conversation && items[$0].roomArchiveUID == uid
        }
        guard matches.count == 1 else { return nil }
        if let roomID = roomStanzaID, let other = items[matches[0]].roomStanzaID, roomID != other { return nil }
        return matches[0]
    }

    /// An authored alias can identify a reply only for a verified author
    /// and only when it selects exactly one candidate.
    func roomAuthoredMatch<Items: Collection>(in items: Items) -> Items.Index? where Items.Element == TimelineItem {
        guard conversation.isRoom, let author = authorJID else { return nil }
        let roomID = roomStanzaID
        let authoredIDs = Set([identity.originID, identity.messageID].compactMap { $0 })
        guard !authoredIDs.isEmpty else { return nil }

        let matches = items.indices.filter { index in
            let existing = items[index]
            guard existing.conversation == conversation, existing.authorJID == author else { return false }
            // Authoritative room IDs take precedence over sender aliases.
            if roomID != nil, existing.roomStanzaID != nil { return false }
            let existingIDs = [existing.identity.originID, existing.identity.messageID].compactMap { $0 }
            return !authoredIDs.isDisjoint(with: existingIDs)
        }
        return matches.count == 1 ? matches[0] : nil
    }

    /// Ids another entity may use to point at this row. In a room only the
    /// room-assigned stanza id: every other id is sender-controlled and
    /// could collide with, and so hijack or block, another occupant's row.
    public var targetableIDs: Set<String> {
        if conversation.isRoom {
            return roomStanzaID.map { [$0] } ?? []
        }
        return identity.all
    }

    /// The id a pin is asked for by (`urn:waddle:pin:0`): in a room the
    /// room-assigned stanza id; in 1:1 the message's `@id`, the one id
    /// both people have for it and the one the server names a pin by, or
    /// for a message that has none the archive's id, which the server
    /// also knows it by. The server does not look a pin up by origin-id.
    public var pinTargetID: String? {
        conversation.isRoom ? roomStanzaID : identity.messageID ?? identity.stanzaID?.id
    }

    /// Every id a pin of this message may be named by. In 1:1 the server
    /// names it by `@id`; for a message that has none it falls back to an
    /// archive id and then to the origin-id.
    public var pinnedIDs: [String] {
        if conversation.isRoom { return roomStanzaID.map { [$0] } ?? [] }
        return [identity.messageID, identity.originID].compactMap(\.self) + identity.stanzaIDs.map(\.id)
    }

    /// The id for reactions (XEP-0444) and replies (XEP-0461): in a
    /// room strictly the room-assigned stanza id; in 1:1 the origin-id if
    /// present, else `@id`. Our own server's archive id never reached the
    /// peer, so it is never used in 1:1.
    public var actionTargetID: String? {
        if conversation.isRoom {
            return roomStanzaID
        }
        return identity.originID ?? identity.messageID
    }

    /// XEP-0424 retraction target: the room-assigned stanza id in a room,
    /// the original `@id` in 1:1.
    public var retractionTargetID: String? {
        if conversation.isRoom {
            return roomStanzaID
        }
        return identity.messageID ?? identity.originID
    }

    /// XEP-0308 targets the `@id` of the original message.
    public var correctionTargetID: String? {
        identity.messageID ?? identity.originID
    }

    /// The XEP-0201 thread a "reply in thread" joins: the row's own thread,
    /// else a new thread rooted at this row.
    public var threadRootID: String? {
        message.thread ?? actionTargetID
    }

    /// The thread this row is a reply in; nil for a row of the main feed.
    public var containingThreadID: String? {
        isFeedVisible ? nil : message.thread
    }

    /// The thread "Reply in thread" opens from this row. In the main feed
    /// (`openThread` nil) that is `threadRootID`. Inside the thread
    /// `openThread`, a reply starts or joins the sub-thread rooted at it;
    /// any other row (the thread's own root) offers none.
    public func threadTarget(inside openThread: String?) -> String? {
        guard let openThread else { return threadRootID }
        return containingThreadID == openThread ? actionTargetID : nil
    }

    /// Author JID for a XEP-0461 reply: the occupant JID in a room, the
    /// bare JID in 1:1.
    public var replyAuthor: JID? {
        guard let from = message.from else { return nil }
        if conversation.isRoom { return from }
        return JID(bare: from.bare, resource: nil)
    }
}
