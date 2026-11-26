import Observation

/// The account's XEP-0191 block list, in snapshot and push order.
@MainActor
@Observable
public final class BlockListStore {
    public enum Load: Equatable, Sendable {
        case idle
        case loading
        case loaded
        case failed
    }

    public private(set) var blockedJIDs: [JID] = []
    public internal(set) var load: Load = .idle

    @ObservationIgnored private var version = 0

    public init() {}

    public func isBlocked(_ person: BareJID) -> Bool {
        blockedJIDs.contains { jid in
            jid.resource == nil && (jid.bare == person ||
                (jid.bare.localpart == nil && jid.bare.domain == person.domain))
        }
    }

    public func versionBeforeFetch() -> Int { version }

    /// Keeps changes that arrived while the snapshot was in flight.
    public func seed(_ jids: [JID], fetchedAt: Int) {
        guard version == fetchedAt else { return }
        var seen: Set<JID> = []
        blockedJIDs = jids.filter { seen.insert($0).inserted }
    }

    public func apply(_ change: BlockListChange) {
        version += 1
        switch change {
        case let .blocked(jids):
            var seen = Set(blockedJIDs)
            blockedJIDs += jids.filter { seen.insert($0).inserted }
        case let .unblocked(jids):
            let removed = Set(jids)
            blockedJIDs.removeAll { removed.contains($0) }
        case .unblockedAll:
            blockedJIDs.removeAll()
        }
    }

    public func clear() {
        version += 1
        blockedJIDs.removeAll()
        load = .idle
    }
}
