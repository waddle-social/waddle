/// Mutable setup machinery stays on the call slot's actor, outside observation.
@MainActor final class GroupCallControl {
    struct Accept {
        let room: BareJID
        let waiter: GroupCallWaiter<CallJoin>
    }
    let mixer: BareJID?
    let echoTimeout: Duration
    let peersTimeout: Duration
    let acceptTimeout: Duration
    var handRevision = 0
    /// The hand state of the last active presence that reached the room in
    /// this call; what a failed toggle falls back to.
    var advertisedHand = false
    var accepts: [String: Accept] = [:]
    /// Bounded, session-local correlation for accepts first arriving after cancellation.
    var abandonedAccepts: [String: BareJID] = [:]
    private var abandonedOrder: [String] = []
    var presenceTail: Task<Void, Never>?

    init(mixer: BareJID?, echoTimeout: Duration, peersTimeout: Duration, acceptTimeout: Duration) {
        self.mixer = mixer
        self.echoTimeout = echoTimeout
        self.peersTimeout = peersTimeout
        self.acceptTimeout = acceptTimeout
    }

    func clear() {
        let pending = accepts.values
        accepts.removeAll()
        abandonedAccepts.removeAll()
        abandonedOrder.removeAll()
        for accept in pending { accept.waiter.cancel() }
        // Keep the tail: an already-written presence can still be in flight.
    }

    func abandonAccept(_ sid: String, room: BareJID) {
        abandonedAccepts[sid] = room
        abandonedOrder.removeAll { $0 == sid }
        abandonedOrder.append(sid)
        if abandonedOrder.count > 256 {
            abandonedAccepts.removeValue(forKey: abandonedOrder.removeFirst())
        }
    }

    /// Enqueue before suspending; MainActor alone cannot order async writes.
    func serializePresence(_ operation: @escaping @MainActor () async -> Bool?) async -> Bool? {
        let previous = presenceTail
        let task = Task { @MainActor in
            await previous?.value
            return await operation()
        }
        presenceTail = Task { _ = await task.value }
        return await task.value
    }
}
