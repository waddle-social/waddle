import Foundation

extension SessionCoordinator {
    /// Asks whether the account owns this community. The port answers "no"
    /// when it cannot ask, so only a "yes" is taken: an owner stays one
    /// until sign-out, and the server checks every admin request anyway.
    /// Asked once connected; every refresh of Events asks again.
    func refreshCommunityOwnership() async {
        guard !isStopped, connection == .online else { return }
        let epoch = connectionEpoch
        guard await port.isCommunityOwner(), epoch == connectionEpoch else { return }
        isCommunityOwner = true
    }

    /// Everyone with an account on this server, for the admin console.
    /// Each request fails with `PortError`: `rejected` for an account that
    /// does not own the community, `notConnected` while offline.
    public func adminUsersList() -> AdminList<AdminUser> {
        AdminList { [weak self] prefix, cursor in
            guard let self else { throw PortError.notConnected }
            return try await self.whileOnline {
                try await self.port.adminUsers(matching: prefix, after: cursor)
            }
        }
    }

    /// The spaces of this community, for the admin console.
    public func adminSpacesList() -> AdminList<AdminSpace> {
        AdminList { [weak self] prefix, cursor in
            guard let self else { throw PortError.notConnected }
            return try await self.whileOnline {
                try await self.port.adminSpaces(matching: prefix, after: cursor)
            }
        }
    }

    /// Everyone in `space`, with their role. It is not searched.
    public func adminSpaceMembersList(of space: AdminSpace) -> AdminList<AdminSpaceMember> {
        AdminList { [weak self] _, cursor in
            guard let self else { throw PortError.notConnected }
            return try await self.whileOnline {
                try await self.port.adminSpaceMembers(of: space, after: cursor)
            }
        }
    }

    // Home, the sidebar and the switcher show spaces from the directory,
    // so each change to a space is followed by a fresh discovery.

    public func createAdminSpace(_ draft: AdminSpaceDraft) async throws {
        try await whileOnline { try await port.createAdminSpace(draft) }
        await refreshDirectory(joiningOnlyNewRooms: true)
    }

    /// Answers with the space as the server now holds it.
    public func updateAdminSpace(_ space: AdminSpace, to draft: AdminSpaceDraft) async throws -> AdminSpace {
        let updated = try await whileOnline { try await port.updateAdminSpace(space, to: draft) }
        await refreshDirectory(joiningOnlyNewRooms: true)
        return updated
    }

    /// Deletes the space and the rooms that belong to it.
    ///
    /// The server deletes some of the space's rooms and only unlinks
    /// others, and its answer does not say which. Joining a deleted room
    /// could make it again, so no room the space listed is joined until
    /// a discovery lists it again; one is asked for at once. A room
    /// missing from a discovery is not taken as deleted, only as not
    /// known to be there: nothing of it is cleared.
    public func deleteAdminSpace(_ space: AdminSpace) async throws {
        try await whileOnline { try await port.deleteAdminSpace(space) }
        let rooms = directory.channels(in: space.node).map(\.roomJID)
        roomsOfDeletedSpaces.formUnion(rooms)
        onDemandRooms.subtract(rooms)
        await refreshDirectory(joiningOnlyNewRooms: true)
    }

    /// Answers with the member and the role the server gave them.
    @discardableResult
    public func setAdminSpaceRole(
        _ role: AdminSpaceRole, of member: BareJID, in space: AdminSpace
    ) async throws -> AdminSpaceMember {
        try await whileOnline { try await port.setAdminSpaceRole(role, of: member, in: space) }
    }

    // MARK: Rooms

    /// Every room of this community, or of one space, for the admin
    /// console.
    public func adminRoomsList() -> AdminList<AdminRoom> {
        AdminList(narrowedBySpace: { [weak self] prefix, space, cursor in
            guard let self else { throw PortError.notConnected }
            return try await self.whileOnline {
                try await self.port.adminRooms(matching: prefix, in: space, after: cursor)
            }
        })
    }

    /// The people in `room` now. It is not searched.
    public func adminRoomOccupantsList(of room: AdminRoom) -> AdminList<AdminRoomOccupant> {
        AdminList { [weak self] _, cursor in
            guard let self else { throw PortError.notConnected }
            return try await self.whileOnline {
                try await self.port.adminRoomOccupants(of: room.jid, after: cursor)
            }
        }
    }

    /// The people with a lasting role in `room`. It is not searched.
    public func adminRoomMembersList(of room: AdminRoom) -> AdminList<AdminRoomMember> {
        AdminList { [weak self] _, cursor in
            guard let self else { throw PortError.notConnected }
            return try await self.whileOnline {
                try await self.port.adminRoomMembers(of: room.jid, after: cursor)
            }
        }
    }

    public func createAdminRoom(_ draft: AdminRoomDraft) async throws {
        try await whileOnline { try await port.createAdminRoom(draft) }
        await refreshDirectory(joiningOnlyNewRooms: true)
    }

    /// Answers with the room as the server now holds it.
    public func updateAdminRoom(_ room: AdminRoom, to draft: AdminRoomDraft) async throws -> AdminRoom {
        let updated = try await whileOnline { try await port.updateAdminRoom(room, to: draft) }
        await refreshDirectory(joiningOnlyNewRooms: true)
        return updated
    }

    /// Takes someone out of a room. They can come back, unless the room is
    /// for members only: there the server also ends their membership. A
    /// blank reason is none.
    public func removeFromAdminRoom(_ occupant: BareJID, room: AdminRoom, reason: String? = nil) async throws {
        let reason = Self.adminReason(reason)
        try await whileOnline { try await port.removeFromAdminRoom(occupant, room: room.jid, reason: reason) }
    }

    /// Answers with the person, the role the server gave them, and the
    /// reason given. A blank reason is none.
    @discardableResult
    public func setAdminRoomAffiliation(
        _ affiliation: RoomAffiliation, of member: BareJID, in room: AdminRoom, reason: String? = nil
    ) async throws -> AdminRoomMember {
        let reason = Self.adminReason(reason)
        return try await whileOnline {
            try await port.setAdminRoomAffiliation(affiliation, of: member, in: room.jid, reason: reason)
        }
    }

    private static func adminReason(_ reason: String?) -> String? {
        guard let reason = reason?.trimmingCharacters(in: .whitespacesAndNewlines), !reason.isEmpty else { return nil }
        return reason
    }
}
