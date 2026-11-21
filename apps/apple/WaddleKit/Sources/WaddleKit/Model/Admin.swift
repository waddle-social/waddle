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
    public let address: String
    public let node: String
    public let name: String
    public let description: String?
    public let roomCount: Int
    public let memberCount: Int

    public var id: String { node }

    public init(address: String, node: String, name: String, description: String?, roomCount: Int, memberCount: Int) {
        self.address = address
        self.node = node
        self.name = name
        self.description = description
        self.roomCount = roomCount
        self.memberCount = memberCount
    }
}

/// The name and description to give a space, as the server would take them.
public struct AdminSpaceDraft: Hashable, Sendable {
    /// The server takes one to this many characters.
    public static let maxNameLength = 80

    public let name: String
    public let description: String?

    /// Nil when the name, trimmed, is blank or too long.
    public init?(name: String, description: String) {
        let name = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty, name.count <= Self.maxNameLength else { return nil }
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
