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

    public func createAdminSpace(_ draft: AdminSpaceDraft) async throws {
        try await whileOnline { try await port.createAdminSpace(draft) }
    }

    /// Answers with the space as the server now holds it.
    public func updateAdminSpace(_ space: AdminSpace, to draft: AdminSpaceDraft) async throws -> AdminSpace {
        try await whileOnline { try await port.updateAdminSpace(space, to: draft) }
    }

    /// Deletes the space and every room in it.
    public func deleteAdminSpace(_ space: AdminSpace) async throws {
        try await whileOnline { try await port.deleteAdminSpace(space) }
    }

    /// Answers with the member and the role the server gave them.
    @discardableResult
    public func setAdminSpaceRole(
        _ role: AdminSpaceRole, of member: BareJID, in space: AdminSpace
    ) async throws -> AdminSpaceMember {
        try await whileOnline { try await port.setAdminSpaceRole(role, of: member, in: space) }
    }
}
