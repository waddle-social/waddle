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

    func adminSpaces(matching prefix: String?, after cursor: String?) async throws -> AdminPage<AdminSpace> {
        let page = try await mappingAdminErrors {
            try await client.adminSpacesList(args: WaddleAdminSpacesListArgs(
                prefix: prefix, pageSize: Self.adminPageSize, afterCursor: cursor
            ))
        }
        // A space whose service does not parse is skipped, not fatal.
        let spaces = page.entries.compactMap { entry in
            FFIInbound.bareJID(entry.spaceJid).map {
                AdminSpace(
                    service: $0,
                    node: entry.spaceNode,
                    name: entry.name,
                    description: Self.nonEmpty(entry.description),
                    roomCount: Int(entry.channelCount),
                    memberCount: Int(entry.memberCount)
                )
            }
        }
        return AdminPage(rows: spaces, nextCursor: page.nextCursor)
    }

    // A change the server refuses is `rejected` whatever its reason: only
    // the lists read `rejected` as "not an owner".

    func createAdminSpace(_ draft: AdminSpaceDraft) async throws {
        _ = try await mappingPortErrors {
            try await client.adminSpacesCreate(args: WaddleAdminSpacesCreateArgs(
                name: draft.name, description: draft.description, iconUrl: nil
            ))
        }
    }

    /// The server leaves what is not sent as it is, so the icon stays. Its
    /// answer has no counts; they are the ones `space` had.
    func updateAdminSpace(_ space: AdminSpace, to draft: AdminSpaceDraft) async throws -> AdminSpace {
        let updated = try await mappingPortErrors {
            try await client.adminSpacesUpdate(args: WaddleAdminSpacesUpdateArgs(
                spaceJid: space.service.description, spaceNode: space.node,
                name: draft.name, description: draft.description, iconUrl: nil
            ))
        }
        return AdminSpace(
            service: space.service,
            node: space.node,
            name: updated.name,
            description: Self.nonEmpty(updated.description),
            roomCount: space.roomCount,
            memberCount: space.memberCount
        )
    }

    func deleteAdminSpace(_ space: AdminSpace) async throws {
        try await mappingPortErrors {
            try await client.adminSpacesDelete(spaceJid: space.service.description, spaceNode: space.node)
        }
    }

    /// A member whose address does not parse is skipped, not fatal.
    func adminSpaceMembers(of space: AdminSpace, after cursor: String?) async throws -> AdminPage<AdminSpaceMember> {
        let page = try await mappingAdminErrors {
            try await client.adminSpacesMembers(args: WaddleAdminSpacesMembersArgs(
                spaceJid: space.service.description, spaceNode: space.node,
                pageSize: Self.adminPageSize, afterCursor: cursor
            ))
        }
        let members = page.entries.compactMap { entry in
            FFIInbound.bareJID(entry.jid).map { AdminSpaceMember(jid: $0, role: Self.role(entry.role)) }
        }
        return AdminPage(rows: members, nextCursor: page.nextCursor)
    }

    /// `rejected` means the server refused this role to this person.
    func setAdminSpaceRole(_ role: AdminSpaceRole, of member: BareJID, in space: AdminSpace) async throws -> AdminSpaceMember {
        let result: WaddleAdminSpacesSetRoleResult
        do {
            result = try await client.adminSpacesSetRole(
                spaceJid: space.service.description, spaceNode: space.node,
                memberJid: member.description, role: Self.role(role)
            )
        } catch {
            throw Self.roleError(error)
        }
        return AdminSpaceMember(jid: member, role: Self.role(result.role))
    }

    // MARK: Rooms

    /// A room whose address does not parse is skipped, not fatal.
    func adminRooms(matching prefix: String?, after cursor: String?) async throws -> AdminPage<AdminRoom> {
        let page = try await mappingAdminErrors {
            try await client.adminChannelsList(args: WaddleAdminChannelsListArgs(
                spaceJid: nil, spaceNode: nil, prefix: prefix, pageSize: Self.adminPageSize, afterCursor: cursor
            ))
        }
        let rooms = page.entries.compactMap { entry in
            FFIInbound.bareJID(entry.channelJid).map {
                AdminRoom(
                    jid: $0,
                    name: entry.name,
                    description: Self.nonEmpty(entry.topic),
                    kind: Self.roomKind(entry.channelType),
                    access: AdminRoomAccess(isListed: entry.isPublic, isMembersOnly: entry.membersOnly),
                    presentCount: Int(entry.occupantCount),
                    // The server counts each role apart.
                    memberCount: Int(entry.ownerCount + entry.adminCount + entry.memberCount)
                )
            }
        }
        return AdminPage(rows: rooms, nextCursor: page.nextCursor)
    }

    func createAdminRoom(_ draft: AdminRoomDraft) async throws {
        _ = try await mappingPortErrors {
            try await client.adminChannelsCreate(args: WaddleAdminChannelsCreateArgs(
                name: draft.name, topic: draft.description, channelType: Self.wire(draft.kind),
                spaceJid: draft.space?.service.description, spaceNode: draft.space?.node,
                isPublic: draft.access.isListed, membersOnly: draft.access.isMembersOnly
            ))
        }
    }

    /// The server leaves what is not sent as it is, and refuses a kind it
    /// does not make, so such a kind is not sent. Its answer has no
    /// counts; they are the ones `room` had.
    func updateAdminRoom(_ room: AdminRoom, to draft: AdminRoomDraft) async throws -> AdminRoom {
        let updated = try await mappingPortErrors {
            try await client.adminChannelsUpdate(args: WaddleAdminChannelsUpdateArgs(
                channelJid: room.jid.description, name: draft.name, topic: draft.description,
                channelType: Self.wire(draft.kind),
                isPublic: draft.access.isListed, membersOnly: draft.access.isMembersOnly
            ))
        }
        return AdminRoom(
            jid: room.jid,
            name: updated.name,
            description: Self.nonEmpty(updated.topic),
            kind: Self.roomKind(updated.channelType),
            access: AdminRoomAccess(isListed: updated.isPublic, isMembersOnly: updated.membersOnly),
            presentCount: room.presentCount,
            memberCount: room.memberCount
        )
    }

    func deleteAdminRoom(_ room: BareJID) async throws {
        try await mappingPortErrors {
            try await client.adminChannelsDelete(channelJid: room.description)
        }
    }

    /// The server names each person by the address of the device they are
    /// in the room from. Someone whose address does not parse is skipped,
    /// not fatal.
    func adminRoomOccupants(of room: BareJID, after cursor: String?) async throws -> AdminPage<AdminRoomOccupant> {
        let page = try await mappingAdminErrors {
            try await client.adminChannelsOccupants(args: WaddleAdminChannelsOccupantsArgs(
                channelJid: room.description, pageSize: Self.adminPageSize, afterCursor: cursor
            ))
        }
        let occupants = page.entries.compactMap { entry in
            Self.person(entry.realJid).map {
                AdminRoomOccupant(
                    jid: $0, nick: entry.nick,
                    role: FFIInbound.roomRole(entry.role),
                    affiliation: FFIInbound.roomAffiliation(entry.affiliation)
                )
            }
        }
        return AdminPage(rows: occupants, nextCursor: page.nextCursor)
    }

    func removeFromAdminRoom(_ occupant: BareJID, room: BareJID) async throws {
        _ = try await mappingPortErrors {
            try await client.adminChannelsKick(
                channelJid: room.description, occupantJid: occupant.description, reason: nil
            )
        }
    }

    func adminRoomMembers(of room: BareJID, after cursor: String?) async throws -> AdminPage<AdminRoomMember> {
        let page = try await mappingAdminErrors {
            try await client.adminChannelsAffiliations(args: WaddleAdminChannelsAffiliationsArgs(
                channelJid: room.description, filter: nil, pageSize: Self.adminPageSize, afterCursor: cursor
            ))
        }
        let members = page.entries.compactMap { entry in
            FFIInbound.bareJID(entry.jid).map {
                AdminRoomMember(
                    jid: $0,
                    affiliation: FFIInbound.roomAffiliation(entry.affiliation),
                    reason: Self.nonEmpty(entry.reason)
                )
            }
        }
        return AdminPage(rows: members, nextCursor: page.nextCursor)
    }

    func setAdminRoomAffiliation(
        _ affiliation: RoomAffiliation, of member: BareJID, in room: BareJID
    ) async throws -> AdminRoomMember {
        let result = try await mappingPortErrors {
            try await client.adminChannelsSetAffiliation(
                channelJid: room.description, memberJid: member.description,
                affiliation: FFIOutbound.affiliation(affiliation), reason: nil
            )
        }
        return AdminRoomMember(
            jid: member, affiliation: FFIInbound.roomAffiliation(result.affiliation), reason: nil
        )
    }

    /// The person behind an address, with or without a device part.
    static func person(_ address: String) -> BareJID? {
        FFIInbound.jid(address)?.bare ?? FFIInbound.bareJID(address)
    }

    static func roomKind(_ wire: String) -> AdminRoomKind {
        switch wire {
        case "text", "": .text
        case "announcement": .announcement
        case "forum": .forum
        default: .other
        }
    }

    /// Nil for a kind the console does not make: the server refuses any
    /// it does not know, and leaves the kind as it is when none is sent.
    static func wire(_ kind: AdminRoomKind) -> String? {
        switch kind {
        case .text: "text"
        case .announcement: "announcement"
        case .forum: "forum"
        case .other: nil
        }
    }

    /// The server sends an empty string for a description a space lacks.
    static func nonEmpty(_ value: String?) -> String? {
        guard let value, !value.isEmpty else { return nil }
        return value
    }

    static func role(_ role: WaddleSpaceRole) -> AdminSpaceRole {
        switch role {
        case .owner: .owner
        case .admin: .admin
        case .member: .member
        case .none: .none
        }
    }

    static func role(_ role: AdminSpaceRole) -> WaddleSpaceRole {
        switch role {
        case .owner: .owner
        case .admin: .admin
        case .member: .member
        case .none: .none
        }
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

    /// The server answers `not-allowed` when it gives owner or admin only
    /// to accounts of this deployment and its trusted servers; any other
    /// error it answers with is a failure that can be tried again.
    static func roleError(_ error: Error) -> PortError {
        if case let WaddleError.Stanza(condition, _) = error, condition != "not-allowed" { return .failed }
        return FFIOutbound.portError(error)
    }

    static func adminError(_ error: Error) -> PortError {
        if case let WaddleError.Stanza(condition, _) = error, condition != "forbidden" { return .failed }
        return FFIOutbound.portError(error)
    }
}
