import Foundation
import WaddleKit

/// The admin console: `urn:waddle:admin:*` commands to the account's server.
extension FFIXmppPort {
    /// Web asks for the same page size.
    private static var adminPageSize: UInt32 { 50 }

    /// A user whose address does not parse is skipped, not fatal.
    func adminUsers(matching prefix: String?, after cursor: String?) async throws -> AdminUsersPage {
        let page = try await mappingPortErrors {
            try await client.adminUsersList(prefix: prefix, pageSize: Self.adminPageSize, afterCursor: cursor)
        }
        let users = page.entries.compactMap { entry in
            FFIInbound.bareJID(entry.jid).map {
                AdminUser(jid: $0, displayName: entry.displayName, isOwner: entry.hasOwnerHat)
            }
        }
        return AdminUsersPage(users: users, nextCursor: page.nextCursor)
    }
}
