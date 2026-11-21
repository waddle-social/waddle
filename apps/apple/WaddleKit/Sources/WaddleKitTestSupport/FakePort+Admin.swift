import Foundation
import WaddleKit

/// The scripted admin console of a `FakePort`.
@MainActor
final class FakeAdmin {
    /// A page asked for. Members are asked for by their space's `node`, in
    /// `prefix`; rooms narrowed to a space by its `node`, in `space`.
    struct PageRequest: Hashable {
        var prefix: String?
        var cursor: String?
        var space: String?
    }

    enum SpaceWrite: Equatable {
        case create(AdminSpaceDraft)
        case update(node: String, AdminSpaceDraft)
        case delete(node: String)
        case setRole(node: String, member: BareJID, AdminSpaceRole)
    }

    enum RoomWrite: Equatable {
        case create(AdminRoomDraft)
        case update(BareJID, AdminRoomDraft)
        case delete(BareJID)
        case remove(BareJID, BareJID)
        case setAffiliation(BareJID, BareJID, RoomAffiliation)
    }

    /// What each request answers; one not scripted answers an empty page.
    /// Who is in a room and its roles are asked for by the name part of
    /// its address, in `prefix`.
    var rooms: [PageRequest: Result<AdminPage<AdminRoom>, PortError>] = [:]
    var roomsRequests: [PageRequest] = []
    /// A held request answers with what was scripted when it began.
    let roomsFetches = HeldCalls()
    var occupants: [PageRequest: Result<AdminPage<AdminRoomOccupant>, PortError>] = [:]
    var roomMembers: [PageRequest: Result<AdminPage<AdminRoomMember>, PortError>] = [:]
    var roomWrites: [RoomWrite] = []

    fileprivate func write(_ write: RoomWrite) throws {
        roomWrites.append(write)
        if let writeFailure { throw writeFailure }
    }

    fileprivate func page<Row>(_ script: [PageRequest: Result<AdminPage<Row>, PortError>], _ prefix: String?, _ cursor: String?) throws -> AdminPage<Row> {
        try (script[PageRequest(prefix: prefix, cursor: cursor)] ?? .success(AdminPage(rows: [], nextCursor: nil))).get()
    }

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

    func adminRooms(matching prefix: String?, in space: AdminSpace?, after cursor: String?) async throws -> AdminPage<AdminRoom> {
        let request = FakeAdmin.PageRequest(prefix: prefix, cursor: cursor, space: space?.node)
        admin.roomsRequests.append(request)
        let answer = admin.rooms[request] ?? .success(AdminPage(rows: [], nextCursor: nil))
        await admin.roomsFetches.wait()
        return try answer.get()
    }

    func createAdminRoom(_ draft: AdminRoomDraft) async throws {
        try admin.write(.create(draft))
    }

    func updateAdminRoom(_ room: AdminRoom, to draft: AdminRoomDraft) async throws -> AdminRoom {
        try admin.write(.update(room.jid, draft))
        return AdminRoom(
            jid: room.jid, name: draft.name, description: draft.description ?? room.description,
            kind: draft.kind.canBeChosen ? draft.kind : room.kind,
            access: draft.access,
            presentCount: room.presentCount, memberCount: room.memberCount
        )
    }

    func deleteAdminRoom(_ room: BareJID) async throws {
        try admin.write(.delete(room))
    }

    func adminRoomOccupants(of room: BareJID, after cursor: String?) async throws -> AdminPage<AdminRoomOccupant> {
        try admin.page(admin.occupants, room.localpart, cursor)
    }

    func removeFromAdminRoom(_ occupant: BareJID, room: BareJID) async throws {
        try admin.write(.remove(room, occupant))
    }

    func adminRoomMembers(of room: BareJID, after cursor: String?) async throws -> AdminPage<AdminRoomMember> {
        try admin.page(admin.roomMembers, room.localpart, cursor)
    }

    func setAdminRoomAffiliation(
        _ affiliation: RoomAffiliation, of member: BareJID, in room: BareJID
    ) async throws -> AdminRoomMember {
        try admin.write(.setAffiliation(room, member, affiliation))
        return AdminRoomMember(jid: member, affiliation: affiliation, reason: nil)
    }
}
