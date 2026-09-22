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
    /// Local send not yet reflected or archived.
    public var isLocalEcho: Bool
    /// When this client first saw the row. Live stanzas carry no timestamp,
    /// so this is their display time.
    public let receivedAt: Date

    public init(
        id: String,
        conversation: ConversationID,
        isMine: Bool,
        message: WireMessage,
        body: String,
        isEdited: Bool = false,
        tombstone: Tombstone? = nil,
        reactions: [ReactionGroup] = [],
        isLocalEcho: Bool = false,
        receivedAt: Date = Date()
    ) {
        self.id = id
        self.conversation = conversation
        self.isMine = isMine
        self.message = message
        self.body = body
        self.isEdited = isEdited
        self.tombstone = tombstone
        self.reactions = reactions
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

    /// Author key used for grouping: occupant JID in rooms, bare JID in 1:1.
    public var authorKey: String {
        guard let from = message.from else { return "" }
        return conversation.isRoom ? from.description : from.bare.description
    }

    /// Thread replies render only inside their thread.
    public var isFeedVisible: Bool {
        guard let thread = message.thread else { return true }
        return identity.all.contains(thread)
    }

    /// The id other clients know this message by, for reactions,
    /// retractions, replies and pins: in a room strictly the room-assigned
    /// XEP-0359 stanza id (`by` must be the room); in 1:1 strictly the
    /// author-assigned id, because our server's archive id never reached
    /// the peer.
    public var actionTargetID: String? {
        if conversation.isRoom {
            return identity.stanzaID(assignedBy: conversation.jid)
        }
        return identity.originID ?? identity.messageID
    }

    /// XEP-0308 targets the author-assigned id of the original send.
    public var correctionTargetID: String? {
        identity.originID ?? identity.messageID
    }

    /// The XEP-0201 thread a "reply in thread" joins: the row's own thread,
    /// else a new thread rooted at this row.
    public var threadRootID: String? {
        message.thread ?? actionTargetID
    }

    /// Author JID for a XEP-0461 reply: the occupant JID in a room, the
    /// bare JID in 1:1.
    public var replyAuthor: JID? {
        guard let from = message.from else { return nil }
        if conversation.isRoom { return from }
        return JID(bare: from.bare, resource: nil)
    }
}
