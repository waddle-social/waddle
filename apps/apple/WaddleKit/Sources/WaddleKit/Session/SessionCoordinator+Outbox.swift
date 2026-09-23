import Foundation

/// The outbox survives the app being killed: every queued, in-flight,
/// unconfirmed or failed own send is saved after each change and restored,
/// local echo included, when the session starts.
///
/// Delivery is at-least-once. A message written to the stream but neither
/// XEP-0198 acknowledged nor reflected back before the app died is re-sent
/// under the same client id, its XEP-0359 origin-id. Our own timeline
/// dedupes the copies on it; a recipient that already had the first copy
/// may still show it twice (XEP-0198 §4 accepts the same tradeoff).
extension SessionCoordinator {
    /// Deletes the saved outbox and signs out: unsent messages must not
    /// outlive the account on this device. A plain `stop()` keeps them.
    public func signOut() async {
        await stop()
        outboxStore.remove()
    }

    /// Loads the saved outbox into the session, once per start. While the
    /// storage is unreadable (before the device's first unlock) nothing is
    /// saved either, so a partial outbox never overwrites the saved one; the
    /// next `start()` or `resume()` tries again.
    func restoreOutboxIfNeeded() {
        guard !isOutboxLoaded, !isStopped else { return }
        let entries: [PersistedOutbound]
        do {
            entries = try outboxStore.load()
        } catch {
            OutboxLog.error("Outbox unavailable: \(error)")
            return
        }
        isOutboxLoaded = true
        savedOutbox = entries
        restore(entries)
        // Sends made before the load are merged into the saved copy.
        persistOutbox()
        // A late load (on resume) can find the session already ready.
        if isSendReady, !outboundQueue.isEmpty {
            Task { [weak self] in await self?.flushOutboundQueue() }
        }
    }

    /// Saves the outbox if it changed since the last save.
    func persistOutbox() {
        guard isOutboxLoaded else { return }
        let snapshot = outboxSnapshot()
        guard snapshot != savedOutbox else { return }
        do {
            try outboxStore.save(snapshot)
            savedOutbox = snapshot
        } catch {
            OutboxLog.error("Could not save outbox: \(error)")
        }
    }

    /// Written-but-unconfirmed sends first (they went out first), then the
    /// queue in order, then failed sends by compose time.
    private func outboxSnapshot() -> [PersistedOutbound] {
        let unconfirmed = sentOrder.compactMap { sentOutbound[$0] }.filter(isAwaitingConfirmation)
        let pending = (unconfirmed + outboundQueue).map { persisted($0, state: .pending) }
        // A failed send the core replayed on a fresh stream can still be
        // acknowledged, or reflected (its local echo replaced by the
        // server's copy); only a send that stays failed is saved.
        let failed = failedOutbound.values
            .filter { deliveries.state(of: $0.clientID) == .failed && echo(of: $0)?.isLocalEcho == true }
            .map { persisted($0, state: .failed) }
            .sorted { $0.createdAt < $1.createdAt }
        var seen = Set<String>()
        return (pending + failed).filter { seen.insert($0.message.clientID).inserted }
    }

    private func restore(_ entries: [PersistedOutbound]) {
        let known = Set(outboundQueue.map(\.clientID)).union(failedOutbound.keys)
        let restored = entries.filter { !known.contains($0.message.clientID) }
        for entry in restored.sorted(by: { $0.createdAt < $1.createdAt }) {
            timelines.insertLocalEcho(localEcho(of: entry.message), in: entry.message.conversation, receivedAt: entry.createdAt)
        }
        for entry in restored where entry.state == .failed {
            failedOutbound[entry.message.clientID] = entry.message
            deliveries.restoredFailure(entry.message.clientID)
        }
        let pending = restored.filter { $0.state == .pending }.map(\.message)
        pending.forEach { deliveries.queued($0.clientID) }
        // Restored sends predate anything queued since launch.
        outboundQueue.insert(contentsOf: pending, at: 0)
    }

    /// Written to the stream, but the server has neither acknowledged it
    /// nor sent back its copy (which replaces the local echo).
    private func isAwaitingConfirmation(_ message: OutboundMessage) -> Bool {
        deliveries.state(of: message.clientID) == .sent && echo(of: message)?.isLocalEcho == true
    }

    private func persisted(_ message: OutboundMessage, state: PersistedOutbound.State) -> PersistedOutbound {
        PersistedOutbound(message: message, createdAt: composedAt(message), state: state)
    }

    /// The echo's first-seen time; once a long wait trimmed the echo from
    /// the timeline, the time already saved.
    private func composedAt(_ message: OutboundMessage) -> Date {
        echo(of: message)?.receivedAt
            ?? savedOutbox.first { $0.message.clientID == message.clientID }?.createdAt
            ?? Date()
    }

    private func echo(of message: OutboundMessage) -> TimelineItem? {
        timelines.timeline(for: message.conversation).item(withID: message.clientID)
    }
}
