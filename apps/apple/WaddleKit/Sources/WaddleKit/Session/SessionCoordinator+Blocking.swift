extension SessionCoordinator {
    /// Reads the block list for this connection. A server tells only a
    /// session that has asked, so until this succeeds the list does not
    /// follow changes made elsewhere: it is tried again a few times. A
    /// snapshot overtaken by a change is from before it and is not taken;
    /// the list is asked for again.
    func loadBlockList() async {
        guard !Task.isCancelled else { return }
        let epoch = connectionEpoch
        let before = blockList.load
        blockList.load = .loading
        for attempt in 0..<Self.blockListAttempts {
            if attempt > 0 {
                try? await Task.sleep(for: blockListRetryDelay * attempt)
            }
            let version = blockList.versionBeforeFetch()
            let jids = try? await port.fetchBlockList()
            guard !Task.isCancelled, epoch == connectionEpoch else {
                // This connection is gone; the next one reads the list itself.
                if blockList.load == .loading { blockList.load = before == .loading ? .idle : before }
                return
            }
            if let jids, blockList.seed(jids, fetchedAt: version) {
                blockList.load = .loaded
                return
            }
        }
        blockList.load = .failed
    }

    static let blockListAttempts = 3

    /// The block list as the app shows it, in the list's order.
    public var blockedEntries: [BlockedEntry] {
        blockList.blockedJIDs.map(entry)
    }

    private func entry(_ jid: JID) -> BlockedEntry {
        BlockedEntry(jid) { directory.title(for: .direct($0)) }
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
        let version = blockList.versionBeforeFetch()
        try await port.block([jid])
        guard !isStopped, epoch == connectionEpoch else { return }
        blockList.confirm(.blocked([jid]), sentAt: version)
    }

    /// What blocks `person`, as the app says it; nil when nothing does.
    public func blockedEntry(covering person: BareJID) -> BlockedEntry? {
        blockList.entry(covering: person).map(entry)
    }

    /// Removes the exact entry from the account's block list.
    public func unblock(_ jid: JID) async throws {
        let epoch = connectionEpoch
        let version = blockList.versionBeforeFetch()
        try await port.unblock([jid])
        guard !isStopped, epoch == connectionEpoch else { return }
        blockList.confirm(.unblocked([jid]), sentAt: version)
    }
}
