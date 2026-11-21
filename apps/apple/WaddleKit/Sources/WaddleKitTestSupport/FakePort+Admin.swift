import Foundation
import WaddleKit

/// The scripted admin console of a `FakePort`.
@MainActor
final class FakeAdmin {
    /// A page asked for. Members are asked for by their space's `node`, in
    /// `prefix`.
    struct PageRequest: Hashable {
        var prefix: String?
        var cursor: String?
    }

    enum SpaceWrite: Equatable {
        case create(AdminSpaceDraft)
        case update(node: String, AdminSpaceDraft)
        case delete(node: String)
        case setRole(node: String, member: BareJID, AdminSpaceRole)
    }

    /// What each request answers; one not scripted answers an empty page.
    var users: [PageRequest: Result<AdminPage<AdminUser>, PortError>] = [:]
    var usersRequests: [PageRequest] = []
    /// A held request answers with what was scripted when it began.
    let usersFetches = HeldCalls()

    var spaces: [PageRequest: Result<AdminPage<AdminSpace>, PortError>] = [:]
    var members: [PageRequest: Result<AdminPage<AdminSpaceMember>, PortError>] = [:]
    /// Every change asked for, refused or not.
    var spaceWrites: [SpaceWrite] = []
    /// When set, every change is refused with it.
    var writeFailure: PortError?

    fileprivate func write(_ write: SpaceWrite) throws {
        spaceWrites.append(write)
        if let writeFailure { throw writeFailure }
    }
}

extension FakePort {
    func adminUsers(matching prefix: String?, after cursor: String?) async throws -> AdminPage<AdminUser> {
        let request = FakeAdmin.PageRequest(prefix: prefix, cursor: cursor)
        admin.usersRequests.append(request)
        let answer = admin.users[request] ?? .success(AdminPage(rows: [], nextCursor: nil))
        await admin.usersFetches.wait()
        return try answer.get()
    }

    func adminSpaces(matching prefix: String?, after cursor: String?) async throws -> AdminPage<AdminSpace> {
        let request = FakeAdmin.PageRequest(prefix: prefix, cursor: cursor)
        return try (admin.spaces[request] ?? .success(AdminPage(rows: [], nextCursor: nil))).get()
    }

    func createAdminSpace(_ draft: AdminSpaceDraft) async throws {
        try admin.write(.create(draft))
    }

    func updateAdminSpace(_ space: AdminSpace, to draft: AdminSpaceDraft) async throws -> AdminSpace {
        try admin.write(.update(node: space.node, draft))
        return AdminSpace(
            service: space.service, node: space.node, name: draft.name,
            description: draft.description ?? space.description,
            roomCount: space.roomCount, memberCount: space.memberCount
        )
    }

    func deleteAdminSpace(_ space: AdminSpace) async throws {
        try admin.write(.delete(node: space.node))
    }

    func adminSpaceMembers(of space: AdminSpace, after cursor: String?) async throws -> AdminPage<AdminSpaceMember> {
        let request = FakeAdmin.PageRequest(prefix: space.node, cursor: cursor)
        return try (admin.members[request] ?? .success(AdminPage(rows: [], nextCursor: nil))).get()
    }

    func setAdminSpaceRole(_ role: AdminSpaceRole, of member: BareJID, in space: AdminSpace) async throws -> AdminSpaceMember {
        try admin.write(.setRole(node: space.node, member: member, role))
        return AdminSpaceMember(jid: member, role: role)
    }
}
