import Foundation

/// A XEP-0503 space.
public struct Space: Hashable, Sendable, Identifiable {
    public let id: String
    public let serviceJID: BareJID?
    public let name: String
    public let summary: String?

    public init(id: String, serviceJID: BareJID?, name: String, summary: String?) {
        self.id = id
        self.serviceJID = serviceJID
        self.name = name
        self.summary = summary
    }
}

/// A room the account can see: a space channel or a group DM.
public struct Channel: Hashable, Sendable, Identifiable {
    public enum Kind: Hashable, Sendable {
        case text
        case forum
        case voice
        case other(String)

        public init(wire: String) {
            switch wire.lowercased() {
            case "text", "": self = .text
            case "forum": self = .forum
            case "voice": self = .voice
            default: self = .other(wire)
            }
        }
    }

    public var id: BareJID { roomJID }
    public let roomJID: BareJID
    public let name: String
    public let summary: String?
    public let kind: Kind
    public let position: Int
    public let spaceID: String?
    public let autojoin: Bool
    public let isGroupDM: Bool

    public init(
        roomJID: BareJID,
        name: String,
        summary: String? = nil,
        kind: Kind = .text,
        position: Int = 0,
        spaceID: String? = nil,
        autojoin: Bool = true,
        isGroupDM: Bool = false
    ) {
        self.roomJID = roomJID
        self.name = name
        self.summary = summary
        self.kind = kind
        self.position = position
        self.spaceID = spaceID
        self.autojoin = autojoin
        self.isGroupDM = isGroupDM
    }

    public var conversation: ConversationID { .room(roomJID) }
}

/// The discovered directory of spaces and rooms.
public struct Topology: Hashable, Sendable {
    public let spaces: [Space]
    public let channels: [Channel]

    public init(spaces: [Space], channels: [Channel]) {
        self.spaces = spaces
        self.channels = channels
    }

    public static let empty = Topology(spaces: [], channels: [])
}

/// A XEP-0045 §9.5 affiliation-list member.
public struct RoomMember: Hashable, Sendable, Identifiable {
    public var id: BareJID { jid }
    public let jid: BareJID
    public let nick: String?
    public let affiliation: RoomAffiliation

    public init(jid: BareJID, nick: String?, affiliation: RoomAffiliation) {
        self.jid = jid
        self.nick = nick
        self.affiliation = affiliation
    }
}

/// A XEP-0055 user search result.
public struct UserSearchResult: Hashable, Sendable, Identifiable {
    public var id: BareJID { jid }
    public let jid: BareJID
    public let displayName: String?

    public init(jid: BareJID, displayName: String?) {
        self.jid = jid
        self.displayName = displayName
    }
}

/// A XEP-0430 inbox row.
public struct InboxEntry: Hashable, Sendable {
    public enum Kind: Hashable, Sendable {
        case direct
        case room

        /// Waddle inbox metadata: `muc` marks a room; absent or any other
        /// value is a direct conversation (the core defaults it to `direct`).
        public init(wire: String) {
            self = wire == "muc" ? .room : .direct
        }
    }

    public let partner: BareJID
    public let kind: Kind
    public let lastStanzaID: String?
    /// Waddle `last-updated`, epoch seconds. Ordering key for freshness.
    public let lastUpdated: Int64?
    public let unread: Int
    public let preview: String?
    /// Set on a room-thread row, which the server keeps beside the room's.
    public let thread: Thread?

    /// A thread row's identity: the XEP-0201 thread id and the Waddle
    /// `thread-title`, the thread root's text.
    public struct Thread: Hashable, Sendable {
        public let id: String
        public let title: String?

        public init(id: String, title: String?) {
            self.id = id
            self.title = title
        }
    }

    public init(
        partner: BareJID,
        kind: Kind,
        lastStanzaID: String?,
        lastUpdated: Int64?,
        unread: Int,
        preview: String?,
        thread: Thread? = nil
    ) {
        self.partner = partner
        self.kind = kind
        self.lastStanzaID = lastStanzaID
        self.lastUpdated = lastUpdated
        self.unread = unread
        self.preview = preview
        self.thread = thread
    }

    public var threadID: String? { thread?.id }
    public var threadTitle: String? { thread?.title }

    /// The same row with a different unread count.
    public func withUnread(_ unread: Int) -> InboxEntry {
        InboxEntry(
            partner: partner,
            kind: kind,
            lastStanzaID: lastStanzaID,
            lastUpdated: lastUpdated,
            unread: unread,
            preview: preview,
            thread: thread
        )
    }

    public var lastUpdatedDate: Date? {
        lastUpdated.map { Date(timeIntervalSince1970: TimeInterval($0)) }
    }
}

/// XEP-0492 per-conversation notification mode.
public enum NotifyMode: Hashable, Sendable, CaseIterable {
    case always
    case onMention
    case never
}

/// A XEP-0313 page.
public struct ArchivePage: Hashable, Sendable {
    public let messages: [WireMessage]
    /// RSM `<first/>`, the cursor for the next older page.
    public let first: String?
    public let isComplete: Bool

    public init(messages: [WireMessage], first: String?, isComplete: Bool) {
        self.messages = messages
        self.first = first
        self.isComplete = isComplete
    }
}

/// Self profile published over PEP.
public struct UserMood: Hashable, Sendable {
    /// XEP-0107 mood value (`happy`, `sad`, …).
    public let value: String
    public let text: String?

    public init(value: String, text: String?) {
        self.value = value
        self.text = text
    }
}
