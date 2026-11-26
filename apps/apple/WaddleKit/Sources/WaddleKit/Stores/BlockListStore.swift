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
        entry(covering: person) != nil
    }

    /// The entry that blocks `person`: their own address, or failing that
    /// their whole server. One of their devices alone does not block them.
    public func entry(covering person: BareJID) -> JID? {
        let whole = blockedJIDs.filter { $0.resource == nil }
        return whole.first { $0.bare == person }
            ?? whole.first { $0.bare.localpart == nil && $0.bare.domain == person.domain }
    }

    public func versionBeforeFetch() -> Int { version }

    /// Takes a snapshot, unless a change arrived while it was in flight:
    /// then it may be from before that change, and is not taken. False
    /// says so, and the list is to be asked for again.
    @discardableResult
    public func seed(_ jids: [JID], fetchedAt: Int) -> Bool {
        guard version == fetchedAt else { return false }
        var seen: Set<JID> = []
        blockedJIDs = jids.filter { seen.insert($0).inserted }
        return true
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
