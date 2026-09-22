import Foundation

/// Server-authoritative XEP-0430 inbox state.
///
/// Hydrate pages and live pushes absolute-set each conversation, guarded by:
/// 1. freshness: an entry older than the stored one (`last-updated`, ties
///    broken on the newest stanza id and unread) is dropped, so a late
///    hydrate never regresses a newer push;
/// 2. a read-clear barrier: a local read snapshots the entry's newest
///    stanza id, and a racing push still naming that id is clamped to zero
///    unread. Only a genuinely newer message voids the barrier.
///
/// It also remembers which newest-message ids the server already counted,
/// so a live message the push covered does not increment unread twice.
@MainActor
public final class InboxStore {
    private struct Key: Hashable {
        let partner: BareJID
        let threadID: String?
    }

    private var entries: [Key: InboxEntry] = [:]
    private var barriers: [Key: String] = [:]
    private var accounted: [BareJID: [String]] = [:]
    private let accountedCap = 20

    public init() {}

    /// Applies an entry; returns the reconciled entry, or nil when stale.
    public func apply(_ entry: InboxEntry) -> InboxEntry? {
        let key = Key(partner: entry.partner, threadID: entry.threadID)
        if isStale(entry, against: entries[key]) {
            return nil
        }
        let reconciled = applyingBarrier(entry, key: key)
        entries[key] = reconciled
        rememberAccounted(reconciled)
        return reconciled
    }

    public func entry(for partner: BareJID, threadID: String? = nil) -> InboxEntry? {
        entries[Key(partner: partner, threadID: threadID)]
    }

    /// Room-thread entries (Activity surface).
    public var threadEntries: [InboxEntry] {
        entries.values.filter { $0.threadID != nil }
    }

    /// Local read: zero unread and arm the barrier.
    public func markRead(_ partner: BareJID, threadID: String? = nil) {
        let key = Key(partner: partner, threadID: threadID)
        guard let existing = entries[key] else { return }
        if let id = existing.lastStanzaID {
            barriers[key] = id
        }
        if existing.unread != 0 {
            entries[key] = InboxEntry(
                partner: existing.partner,
                kind: existing.kind,
                lastStanzaID: existing.lastStanzaID,
                lastUpdated: existing.lastUpdated,
                unread: 0,
                preview: existing.preview,
                threadID: existing.threadID
            )
        }
    }

    /// Whether the server inbox already counted one of `ids` as the
    /// conversation's newest message.
    public func wasAccounted(_ partner: BareJID, ids: Set<String>) -> Bool {
        guard let known = accounted[partner] else { return false }
        return known.contains(where: ids.contains)
    }

    public func clear() {
        entries.removeAll()
        barriers.removeAll()
        accounted.removeAll()
    }

    private func isStale(_ incoming: InboxEntry, against existing: InboxEntry?) -> Bool {
        guard let existing else { return false }
        let incomingUpdated = incoming.lastUpdated ?? .min
        let existingUpdated = existing.lastUpdated ?? .min
        if incomingUpdated < existingUpdated { return true }
        return incomingUpdated == existingUpdated
            && incoming.lastStanzaID != existing.lastStanzaID
            && incoming.unread <= existing.unread
    }

    private func applyingBarrier(_ incoming: InboxEntry, key: Key) -> InboxEntry {
        guard let barrier = barriers[key] else { return incoming }
        guard incoming.lastStanzaID == barrier else {
            barriers[key] = nil
            return incoming
        }
        guard incoming.unread != 0 else { return incoming }
        return InboxEntry(
            partner: incoming.partner,
            kind: incoming.kind,
            lastStanzaID: incoming.lastStanzaID,
            lastUpdated: incoming.lastUpdated,
            unread: 0,
            preview: incoming.preview,
            threadID: incoming.threadID
        )
    }

    private func rememberAccounted(_ entry: InboxEntry) {
        guard let id = entry.lastStanzaID else { return }
        var ids = accounted[entry.partner] ?? []
        ids.removeAll { $0 == id }
        ids.append(id)
        if ids.count > accountedCap {
            ids.removeFirst(ids.count - accountedCap)
        }
        accounted[entry.partner] = ids
    }
}
