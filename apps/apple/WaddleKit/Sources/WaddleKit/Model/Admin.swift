import Foundation

/// Someone with an account on this server, as the admin console lists them.
public struct AdminUser: Hashable, Identifiable, Sendable {
    public let jid: BareJID
    public let displayName: String?
    /// They own this community.
    public let isOwner: Bool

    public var id: BareJID { jid }

    public init(jid: BareJID, displayName: String?, isOwner: Bool) {
        self.jid = jid
        self.displayName = displayName
        self.isOwner = isOwner
    }

    /// Their display name or, without one, the name part of their address.
    public var name: String {
        if let displayName = displayName?.trimmingCharacters(in: .whitespacesAndNewlines), !displayName.isEmpty {
            return displayName
        }
        return jid.localpart ?? jid.description
    }
}

/// One page of a list of the admin console, and where the next one starts.
public struct AdminPage<Row: Sendable>: Sendable {
    public let rows: [Row]
    /// Nil on the last page.
    public let nextCursor: String?

    public init(rows: [Row], nextCursor: String?) {
        self.rows = rows
        self.nextCursor = nextCursor
    }
}

extension AdminPage: Equatable where Row: Equatable {}

/// A space, as the admin console lists it.
public struct AdminSpace: Hashable, Identifiable, Sendable {
    /// The service that holds the space, and its name there. The server
    /// wants both to find it.
    public let service: BareJID
    public let node: String
    public let name: String
    public let description: String?
    public let roomCount: Int
    public let memberCount: Int

    public var id: String { node }

    public init(service: BareJID, node: String, name: String, description: String?, roomCount: Int, memberCount: Int) {
        self.service = service
        self.node = node
        self.name = name
        self.description = description
        self.roomCount = roomCount
        self.memberCount = memberCount
    }

    /// The space after `count` of its members were removed.
    public func removingMembers(_ count: Int) -> AdminSpace {
        AdminSpace(
            service: service, node: node, name: name, description: description,
            roomCount: roomCount, memberCount: max(0, memberCount - count)
        )
    }
}

/// The name and description to give a space, as the server would take them.
public struct AdminSpaceDraft: Hashable, Sendable {
    /// The server takes one to this many characters, counted as it counts
    /// them: one for each Unicode scalar, so some emoji count several.
    public static let maxNameLength = 80

    /// Whether `name`, trimmed, is longer than the server takes.
    public static func isNameTooLong(_ name: String) -> Bool {
        name.trimmingCharacters(in: .whitespacesAndNewlines).unicodeScalars.count > maxNameLength
    }

    public let name: String
    public let description: String?

    /// Nil when the name, trimmed, is blank or too long.
    public init?(name: String, description: String) {
        let name = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty, !Self.isNameTooLong(name) else { return nil }
        let description = description.trimmingCharacters(in: .whitespacesAndNewlines)
        self.name = name
        self.description = description.isEmpty ? nil : description
    }
}

/// What someone may do in a space. `none` takes them out of it.
public enum AdminSpaceRole: Hashable, Sendable, CaseIterable {
    case owner
    case admin
    case member
    case none
}

/// Someone in a space, and their role there.
public struct AdminSpaceMember: Hashable, Identifiable, Sendable {
    public let jid: BareJID
    public let role: AdminSpaceRole

    public var id: BareJID { jid }

    public init(jid: BareJID, role: AdminSpaceRole) {
        self.jid = jid
        self.role = role
    }
}

/// What a room is for, as the admin console can set it.
public enum AdminRoomKind: Hashable, Sendable {
    case text
    case announcement
    case forum
    /// A kind the console does not make, such as a group chat.
    case other(String)

    /// The kinds a room can be made as or changed to.
    public static let choices: [AdminRoomKind] = [.text, .announcement, .forum]

    public init(wire: String) {
        switch wire {
        case "text", "": self = .text
        case "announcement": self = .announcement
        case "forum": self = .forum
        default: self = .other(wire)
        }
    }

    public var wire: String {
        switch self {
        case .text: "text"
        case .announcement: "announcement"
        case .forum: "forum"
        case let .other(wire): wire
        }
    }

    public var canBeChosen: Bool { Self.choices.contains(self) }
}

/// A room, as the admin console lists it.
public struct AdminRoom: Hashable, Identifiable, Sendable {
    public let jid: BareJID
    public let name: String
    public let description: String?
    public let kind: AdminRoomKind
    /// People can find it when they browse rooms.
    public let isListed: Bool
    /// Only its members can join.
    public let isMembersOnly: Bool
    /// How many people are in it now, and how many are members.
    public let presentCount: Int
    public let memberCount: Int

    public var id: BareJID { jid }

    public init(
        jid: BareJID, name: String, description: String?, kind: AdminRoomKind,
        isListed: Bool, isMembersOnly: Bool, presentCount: Int, memberCount: Int
    ) {
        self.jid = jid
        self.name = name
        self.description = description
        self.kind = kind
        self.isListed = isListed
        self.isMembersOnly = isMembersOnly
        self.presentCount = presentCount
        self.memberCount = memberCount
    }

    /// The room after `count` of the people in it were removed.
    public func removingPresent(_ count: Int) -> AdminRoom {
        AdminRoom(
            jid: jid, name: name, description: description, kind: kind,
            isListed: isListed, isMembersOnly: isMembersOnly,
            presentCount: max(0, presentCount - count), memberCount: memberCount
        )
    }
}

/// What to make a room as, or change it to, as the server would take it.
public struct AdminRoomDraft: Hashable, Sendable {
    public let name: String
    public let description: String?
    public let kind: AdminRoomKind
    public let isListed: Bool
    public let isMembersOnly: Bool
    /// The space to make the room in. Not used when changing a room.
    public let space: AdminSpace?

    /// Nil when the name, trimmed, is blank or too long (the limit is the
    /// one for a space). A room that is not listed is for members only:
    /// otherwise anyone who guessed its address could join.
    public init?(
        name: String, description: String, kind: AdminRoomKind,
        isListed: Bool, isMembersOnly: Bool, space: AdminSpace?
    ) {
        let name = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty, !AdminSpaceDraft.isNameTooLong(name) else { return nil }
        let description = description.trimmingCharacters(in: .whitespacesAndNewlines)
        self.name = name
        self.description = description.isEmpty ? nil : description
        self.kind = kind
        self.isListed = isListed
        self.isMembersOnly = isMembersOnly || !isListed
        self.space = space
    }
}

/// Someone in a room right now.
public struct AdminRoomOccupant: Hashable, Identifiable, Sendable {
    public let jid: BareJID
    public let nick: String
    public let role: RoomRole
    public let affiliation: RoomAffiliation

    public var id: BareJID { jid }

    public init(jid: BareJID, nick: String, role: RoomRole, affiliation: RoomAffiliation) {
        self.jid = jid
        self.nick = nick
        self.role = role
        self.affiliation = affiliation
    }
}

/// Someone with a lasting role in a room, here or not; `outcast` is banned.
public struct AdminRoomMember: Hashable, Identifiable, Sendable {
    public let jid: BareJID
    public let affiliation: RoomAffiliation
    /// Why, when whoever set the role said.
    public let reason: String?

    public var id: BareJID { jid }

    public init(jid: BareJID, affiliation: RoomAffiliation, reason: String?) {
        self.jid = jid
        self.affiliation = affiliation
        self.reason = reason
    }
}
