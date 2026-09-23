import Foundation

/// Durable storage for the session's unconfirmed sends, so a queued or
/// failed message survives the app being killed.
@MainActor
public protocol OutboxStore: Sendable {
    /// The saved entries in send order. Empty when nothing is saved, or
    /// when the saved copy was unreadable (it is discarded). Throws when the
    /// storage cannot be read right now (for example before the device's
    /// first unlock); the saved copy is then left untouched.
    func load() throws -> [PersistedOutbound]
    /// Replaces the saved entries; an empty list removes the saved copy.
    func save(_ entries: [PersistedOutbound]) throws
    /// Deletes the saved copy (sign-out).
    func remove()
}

/// Keeps the outbox for the life of the process only.
@MainActor
public final class InMemoryOutboxStore: OutboxStore {
    public private(set) var entries: [PersistedOutbound]

    public init(entries: [PersistedOutbound] = []) {
        self.entries = entries
    }

    public func load() throws -> [PersistedOutbound] {
        entries
    }

    public func save(_ entries: [PersistedOutbound]) throws {
        self.entries = entries
    }

    public func remove() {
        entries.removeAll()
    }
}
