extension SessionCoordinator {
    func loadBlockList() async {
        guard !Task.isCancelled else { return }
        let epoch = connectionEpoch
        let version = blockList.versionBeforeFetch()
        blockList.load = .loading
        let jids = try? await port.fetchBlockList()
        guard !Task.isCancelled, epoch == connectionEpoch else { return }
        if let jids {
            blockList.seed(jids, fetchedAt: version)
            blockList.load = .loaded
        } else {
            blockList.load = .failed
        }
    }

    /// The block list as the app shows it, in the list's order.
    public var blockedEntries: [BlockedEntry] {
        blockList.blockedJIDs.map { jid in
            BlockedEntry(jid) { directory.title(for: .direct($0)) }
        }
    }

    /// Asks for the list again, as after a load that failed.
    public func reloadBlockList() async {
        guard connection == .online else { return }
        await loadBlockList()
    }

    /// Blocks the person's bare address on the account.
    public func block(_ person: BareJID) async throws {
        let jid = JID(bare: person, resource: nil)!
        let epoch = connectionEpoch
        try await port.block([jid])
        // A matching push may already have applied this change.
        guard !isStopped, epoch == connectionEpoch, !blockList.blockedJIDs.contains(jid) else { return }
        blockList.apply(.blocked([jid]))
    }

    /// Removes the exact entry from the account's block list.
    public func unblock(_ jid: JID) async throws {
        let epoch = connectionEpoch
        try await port.unblock([jid])
        guard !isStopped, epoch == connectionEpoch, blockList.blockedJIDs.contains(jid) else { return }
        blockList.apply(.unblocked([jid]))
    }
}
