import Foundation

extension SessionCoordinator {
    /// Asks whether the account owns this community. The port answers "no"
    /// when it cannot ask, so only a "yes" is taken: an owner stays one
    /// until sign-out, and the server checks every admin request anyway.
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
            let page = try await self.whileOnline {
                try await self.port.adminUsers(matching: prefix, after: cursor)
            }
            return (page.users, page.nextCursor)
        }
    }
}
