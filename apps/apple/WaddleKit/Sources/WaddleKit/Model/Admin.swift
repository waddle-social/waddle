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

/// What the server takes as the name of a space or a room.
public enum AdminName {
    /// One to this many characters, counted as the server counts them:
    /// one for each Unicode scalar, so some emoji count several.
    public static let maxLength = 80

    /// Whether `name`, trimmed, is longer than the server takes.
    public static func isTooLong(_ name: String) -> Bool {
        name.trimmingCharacters(in: .whitespacesAndNewlines).unicodeScalars.count > maxLength
    }
}

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

    /// The space after `count` members were added.
    public func addingMembers(_ count: Int) -> AdminSpace {
        removingMembers(-count)
    }
}

/// The name and description to give a space, as the server would take them.
public struct AdminSpaceDraft: Hashable, Sendable {
    public let name: String
    public let description: String?

    /// Nil when the name, trimmed, is blank or too long.
    public init?(name: String, description: String) {
        let name = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty, !AdminName.isTooLong(name) else { return nil }
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
    /// A kind the console does not make. It is shown, and left as it is.
    case other

    /// The kinds a room can be made as or changed to.
    public static let choices: [AdminRoomKind] = [.text, .announcement, .forum]

    public var canBeChosen: Bool { Self.choices.contains(self) }
}

/// Who can get into a room.
public struct AdminRoomAccess: Hashable, Sendable {
    /// People can find it when they browse rooms.
    public var isListed: Bool
    /// Only its members can join.
    public var isMembersOnly: Bool

    /// Anyone can find it and join it.
    public static let open = AdminRoomAccess(isListed: true, isMembersOnly: false)

    public init(isListed: Bool, isMembersOnly: Bool) {
        self.isListed = isListed
        self.isMembersOnly = isMembersOnly
    }
}

/// A room, as the admin console lists it.
public struct AdminRoom: Hashable, Identifiable, Sendable {
    public let jid: BareJID
    public let name: String
    public let description: String?
    public let kind: AdminRoomKind
    public let access: AdminRoomAccess
    /// How many people are in it now, and how many hold a role in it
    /// (owner, admin or member).
    public let presentCount: Int
    public let memberCount: Int

    public var id: BareJID { jid }

    public init(
        jid: BareJID, name: String, description: String?, kind: AdminRoomKind,
        access: AdminRoomAccess, presentCount: Int, memberCount: Int
    ) {
        self.jid = jid
        self.name = name
        self.description = description
        self.kind = kind
        self.access = access
        self.presentCount = presentCount
        self.memberCount = memberCount
    }
}

/// What to make a room as, or change it to, as the server would take it.
public struct AdminRoomDraft: Hashable, Sendable {
    public let name: String
    public let description: String?
    public let kind: AdminRoomKind
    public let access: AdminRoomAccess
    /// The space to make the room in. Not used when changing a room.
    public let space: AdminSpace?

    /// Nil when the name, trimmed, is blank or too long.
    public init?(
        name: String, description: String, kind: AdminRoomKind, access: AdminRoomAccess, space: AdminSpace?
    ) {
        let name = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty, !AdminName.isTooLong(name) else { return nil }
        let description = description.trimmingCharacters(in: .whitespacesAndNewlines)
        self.name = name
        self.description = description.isEmpty ? nil : description
        self.kind = kind
        self.access = access
        self.space = space
    }
}

/// Someone in a room right now, under the name they use there. One person
/// can be in a room twice, from two devices, under two names.
public struct AdminRoomOccupant: Hashable, Identifiable, Sendable {
    public let jid: BareJID
    public let nick: String
    public let role: RoomRole
    public let affiliation: RoomAffiliation

    public var id: String { nick }

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
