import Foundation
import WaddleKit

/// The scripted admin console of a `FakePort`.
@MainActor
final class FakeAdmin {
    struct UsersRequest: Hashable {
        var prefix: String?
        var cursor: String?
    }

    /// What each request answers; one not scripted answers an empty page.
    var users: [UsersRequest: Result<AdminUsersPage, PortError>] = [:]
    var usersRequests: [UsersRequest] = []
    /// A held request answers with what was scripted when it began.
    let usersFetches = HeldCalls()
}

extension FakePort {
    func adminUsers(matching prefix: String?, after cursor: String?) async throws -> AdminUsersPage {
        let request = FakeAdmin.UsersRequest(prefix: prefix, cursor: cursor)
        admin.usersRequests.append(request)
        let answer = admin.users[request] ?? .success(AdminUsersPage(users: [], nextCursor: nil))
        await admin.usersFetches.wait()
        return try answer.get()
    }
}
