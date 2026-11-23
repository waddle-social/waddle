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
    /// The account left the room: no device joins it until the account
    /// rejoins. Kept as the account's own bookmark with autojoin off
    /// (XEP-0402), which is not what `autojoin` holds: that is also off
    /// for rooms a space lists without joining them at sign-in.
    public let hasLeft: Bool
    /// Whether room discovery advertised members-only access, or nil if unknown.
    public private(set) var membersOnly: Bool?
    /// Who may pin, from the room's disco#info, which every occupant can
    /// read; nil if unknown.
    public private(set) var pinPermission: PinPermission?

    public init(
        roomJID: BareJID,
        name: String,
        summary: String? = nil,
        kind: Kind = .text,
        position: Int = 0,
        spaceID: String? = nil,
        autojoin: Bool = true,
        isGroupDM: Bool = false,
        membersOnly: Bool? = nil,
        hasLeft: Bool = false,
        pinPermission: PinPermission? = nil
    ) {
        self.roomJID = roomJID
        self.name = name
        self.summary = summary
        self.kind = kind
        self.position = position
        self.spaceID = spaceID
        self.autojoin = autojoin
        self.isGroupDM = isGroupDM
        self.membersOnly = membersOnly
        self.hasLeft = hasLeft
        self.pinPermission = pinPermission
    }

    public var conversation: ConversationID { .room(roomJID) }

    /// The same room with a different name, summary and pin permission.
    public func with(name: String, summary: String?, pinPermission: PinPermission? = nil) -> Channel {
        Channel(
            roomJID: roomJID, name: name, summary: summary, kind: kind, position: position,
            spaceID: spaceID, autojoin: autojoin, isGroupDM: isGroupDM, membersOnly: membersOnly, hasLeft: hasLeft,
            pinPermission: pinPermission ?? self.pinPermission)
    }

    /// The same room, left or rejoined.
    public func with(hasLeft: Bool) -> Channel {
        Channel(
            roomJID: roomJID, name: name, summary: summary, kind: kind, position: position,
            spaceID: spaceID, autojoin: autojoin, isGroupDM: isGroupDM, membersOnly: membersOnly, hasLeft: hasLeft,
            pinPermission: pinPermission)
    }

    func withKnownMembersOnly(_ value: Bool?) -> Channel {
        var copy = self
        copy.membersOnly = value ?? membersOnly
        return copy
    }

    func withPinPermission(_ value: PinPermission?) -> Channel {
        var copy = self
        copy.pinPermission = value
        return copy
    }
}

/// The discovered directory of spaces and rooms.
public struct Topology: Hashable, Sendable {
    public let spaces: [Space]
    public let channels: [Channel]
    /// False when the account's own bookmarks could not be read: then no
    /// channel's `hasLeft` is known, and each reads false.
    public let ownBookmarksLoaded: Bool

    public init(spaces: [Space], channels: [Channel], ownBookmarksLoaded: Bool = true) {
        self.spaces = spaces
        self.channels = channels
        self.ownBookmarksLoaded = ownBookmarksLoaded
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

/// A bot a room's server declares (XEP-0030 `urn:waddle:room:bots:0`).
/// Bots hold no affiliation and join a room only for one send, so neither
/// the member lists nor the occupants name them.
public struct RoomBot: Hashable, Sendable, Identifiable {
    public var id: BareJID { jid }
    public let jid: BareJID
    /// The display name the service reports, if any.
    public let name: String?

    public init(jid: BareJID, name: String?) {
        self.jid = jid
        self.name = name
    }

    public var displayName: String {
        if let name = name?.trimmingCharacters(in: .whitespacesAndNewlines), !name.isEmpty {
            return name
        }
        return jid.localpart ?? jid.domain
    }
}

/// A XEP-0055 user search result.
public struct UserSearchResult: Hashable, Sendable, Identifiable {
    public var id: BareJID { jid }
    public let jid: BareJID
    /// The Waddle username (the XEP-0055 `<nick/>` field): what an `@`
    /// mention inserts. Falls back to the JID's localpart when blank.
    public let username: String
    public let displayName: String?

    public init(jid: BareJID, username: String? = nil, displayName: String?) {
        self.jid = jid
        let trimmed = username?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        self.username = trimmed.isEmpty ? (jid.localpart ?? jid.domain) : trimmed
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

/// The notification settings the account stores per conversation.
public struct NotifySettings: Hashable, Sendable {
    /// Only conversations with an explicit mode.
    public var modes: [ConversationID: NotifyMode]
    /// Conversations whose push notifications show who wrote and what.
    public var showingMessagesInPush: Set<ConversationID>

    public init(modes: [ConversationID: NotifyMode] = [:], showingMessagesInPush: Set<ConversationID> = []) {
        self.modes = modes
        self.showingMessagesInPush = showingMessagesInPush
    }
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
