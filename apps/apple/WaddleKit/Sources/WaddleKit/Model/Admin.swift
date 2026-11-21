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
