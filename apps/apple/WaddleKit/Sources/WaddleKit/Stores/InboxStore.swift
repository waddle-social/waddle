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
    /// Rows whose last server report counted unread, before any local
    /// read clamped them: the server still needs a `<mark-read/>`.
    private var unreadOnServer: Set<Key> = []
    private var accounted: [BareJID: [String]] = [:]
    private let accountedCap = 20

    public init() {}

    /// Applies an entry; returns the reconciled entry, or nil when stale.
    public func apply(_ entry: InboxEntry) -> InboxEntry? {
        let key = Key(partner: entry.partner, threadID: entry.threadID)
        if isStale(entry, against: entries[key]) {
            return nil
        }
        if entry.unread > 0 {
            unreadOnServer.insert(key)
        } else {
            unreadOnServer.remove(key)
        }
        let reconciled = applyingBarrier(entry, key: key)
        entries[key] = reconciled
        rememberAccounted(reconciled)
        return reconciled
    }

    public func entry(for partner: BareJID, threadID: String? = nil) -> InboxEntry? {
        entries[Key(partner: partner, threadID: threadID)]
    }

    /// Local read: zero unread and arm the barrier. Returns the newest
    /// stanza id the read covers, so a read replayed later can check that
    /// nothing newer arrived in between.
    @discardableResult
    public func markRead(_ partner: BareJID, threadID: String? = nil) -> String? {
        let key = Key(partner: partner, threadID: threadID)
        guard let existing = entries[key] else { return nil }
        if let id = existing.lastStanzaID {
            barriers[key] = id
        }
        if existing.unread != 0 {
            entries[key] = existing.withUnread(0)
        }
        return existing.lastStanzaID
    }

    /// Whether the server last reported unread for the row and has not
    /// taken a read since. A local read zeroes the row but not this.
    public func isUnreadOnServer(_ partner: BareJID, threadID: String? = nil) -> Bool {
        unreadOnServer.contains(Key(partner: partner, threadID: threadID))
    }

    /// The server took a `<mark-read/>` for the row.
    public func serverTookRead(_ partner: BareJID, threadID: String? = nil) {
        unreadOnServer.remove(Key(partner: partner, threadID: threadID))
    }

    /// Drops the read-clear barrier after the server did not take the read,
    /// so the next hydrate or push shows the server's count again.
    public func forgetBarrier(_ partner: BareJID, threadID: String? = nil) {
        barriers[Key(partner: partner, threadID: threadID)] = nil
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
        unreadOnServer.removeAll()
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
        return incoming.withUnread(0)
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
