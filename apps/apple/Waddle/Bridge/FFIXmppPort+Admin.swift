import Foundation
import WaddleKit

/// The admin console: `urn:waddle:admin:*` commands to the account's server.
extension FFIXmppPort {
    /// Web asks for the same page size.
    private static var adminPageSize: UInt32 { 50 }

    /// A user whose address does not parse is skipped, not fatal.
    func adminUsers(matching prefix: String?, after cursor: String?) async throws -> AdminPage<AdminUser> {
        let page = try await mappingAdminErrors {
            try await client.adminUsersList(prefix: prefix, pageSize: Self.adminPageSize, afterCursor: cursor)
        }
        let users = page.entries.compactMap { entry in
            FFIInbound.bareJID(entry.jid).map {
                AdminUser(jid: $0, displayName: entry.displayName, isOwner: entry.hasOwnerHat)
            }
        }
        return AdminPage(rows: users, nextCursor: page.nextCursor)
    }

    /// `rejected` means "not an owner" to the console, so only the
    /// server's `forbidden` becomes it; any other error it answers with is
    /// a failure an owner can try again.
    private func mappingAdminErrors<Value>(_ operation: () async throws -> Value) async throws -> Value {
        do {
            return try await operation()
        } catch {
            throw Self.adminError(error)
        }
    }

    static func adminError(_ error: Error) -> PortError {
        if case let WaddleError.Stanza(condition, _) = error, condition != "forbidden" { return .failed }
        return FFIOutbound.portError(error)
    }
}
