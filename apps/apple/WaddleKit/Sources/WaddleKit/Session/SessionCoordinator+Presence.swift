import Foundation

/// What is known of the account's stored presence mode (ADR-010).
struct PresenceModeSync {
    /// The mode the server is known to hold; nil until one is stored or
    /// read in this session.
    var stored: PresenceMode?
    /// The mode on its way to the server.
    var publishing: PresenceMode?
    /// A pick arrived while another was on its way.
    var needsAnotherPass = false
    /// Moves whenever `stored` is set from outside a publish, so a publish
    /// that was on its way then does not put its older value back.
    var generation = 0
}

/// When this device last saw input.
struct IdleClock {
    /// Nil until the first input or the first read, which starts it.
    var lastActive: Date?
    /// A picked status or a call held auto-away off: idle time counts from
    /// when that ends, not from the input before it.
    var restartsOnNextRead = false
}

struct SentPresence: Equatable {
    let presence: OwnPresence
    let statusText: String?
}

extension SessionCoordinator {
    // MARK: - Picking

    /// Picks a status for the account, or `.automatic` to let each device's
    /// idle time decide again. Shown at once; stored on the server when
    /// online, or on the next connect.
    public func setPresenceMode(_ mode: PresenceMode) async {
        status.mode = mode
        await sendOwnPresence()
        await storePresenceMode()
    }

    /// The RFC 6121 status message; blank removes it.
    public func setStatusText(_ text: String?) async {
        let trimmed = text?.trimmingCharacters(in: .whitespacesAndNewlines)
        status.statusText = (trimmed?.isEmpty ?? true) ? nil : trimmed
        await sendOwnPresence()
    }

    // MARK: - Auto-away

    /// The user did something on this device at `instant` (now when nil).
    /// Reports may come late and out of order: only a later one counts.
    public func noteActivity(at instant: Date? = nil) {
        let instant = instant ?? now()
        if idleClock.lastActive.map({ instant > $0 }) ?? true {
            idleClock.lastActive = instant
        }
    }

    /// Works out what this device should show now and tells the server if
    /// that changed. The host calls it on a timer and after input.
    public func refreshOwnPresence() async {
        await sendOwnPresence()
    }

    func sendOwnPresence() async {
        let presence = ownPresence()
        status.availability = presence.availability
        let next = SentPresence(presence: presence, statusText: status.statusText)
        guard connection == .online, next != sentPresence else { return }
        sentPresence = next
        await port.sendPresence(presence.availability, status: next.statusText, idleSince: presence.idleSince)
    }

    private func ownPresence() -> OwnPresence {
        let time = now()
        guard status.mode == .automatic else {
            idleClock.restartsOnNextRead = true
            return OwnPresence(mode: status.mode, autoAway: .active)
        }
        // Being in a call is proof of presence (ADR-010).
        var lastActive = idleClock.lastActive ?? time
        if idleClock.restartsOnNextRead || calls.state.isEstablished {
            lastActive = time
        }
        idleClock = IdleClock(lastActive: lastActive)
        return OwnPresence(mode: .automatic, autoAway: AutoAway(now: time, lastActive: lastActive))
    }

    // MARK: - The account's stored mode

    /// Stores the picked mode unless the server already holds it. A pick
    /// made while another is on its way is stored after it, never dropped.
    /// A failed attempt leaves `stored` as it was, so the next connect
    /// tries again.
    private func storePresenceMode() async {
        guard !isStopped, connection == .online else { return }
        if presenceModeSync.publishing != nil {
            presenceModeSync.needsAnotherPass = true
            return
        }
        let mode = status.mode
        if presenceModeSync.stored == mode { return }
        presenceModeSync.publishing = mode
        let generation = presenceModeSync.generation
        let taken = (try? await port.publishPresenceMode(mode)) != nil
        presenceModeSync.publishing = nil
        // Another device's pick, or signing out, overtook this publish:
        // what they set stands.
        let overtaken = presenceModeSync.generation != generation
        if taken, !overtaken {
            presenceModeSync.stored = mode
        }
        let again = presenceModeSync.needsAnotherPass || (taken && !overtaken && status.mode != mode)
        presenceModeSync.needsAnotherPass = false
        if again {
            await storePresenceMode()
        }
    }

    /// A pick the server does not hold yet. Before anything is known of
    /// the server, only a picked status counts: a new device is Automatic
    /// by default and must not write that over the account's pick.
    private var hasUnstoredPick: Bool {
        guard let stored = presenceModeSync.stored else { return status.mode != .automatic }
        return status.mode != stored
    }

    /// On connect: a pick made offline wins and is stored; otherwise this
    /// device takes up what the account has stored. If that cannot be
    /// read, the device stays as it was.
    func syncPresenceModeOnConnect() async {
        if hasUnstoredPick {
            await storePresenceMode()
            return
        }
        let before = status.mode
        let generation = presenceModeSync.generation
        let fetched = try? await port.fetchPresenceMode()
        guard !isStopped, !Task.isCancelled else { return }
        // A pick here or on another device arrived while reading: it is
        // newer than what was read.
        guard status.mode == before, presenceModeSync.generation == generation else {
            await storePresenceMode()
            return
        }
        guard let fetched else { return }
        presenceModeSync.stored = fetched
        status.mode = fetched
        await sendOwnPresence()
    }

    /// Another device of the account picked `mode`. Taken up here without
    /// storing it again, or two devices would answer each other forever.
    func presenceModePicked(elsewhere mode: PresenceMode) {
        guard !isStopped else { return }
        // The server's echo of the pick this device is storing right now.
        guard presenceModeSync.publishing != mode else { return }
        presenceModeSync.generation += 1
        presenceModeSync.stored = mode
        guard status.mode != mode else { return }
        status.mode = mode
        Task { await self.sendOwnPresence() }
    }

    /// Signing out is local: the account's pick stays on the server for
    /// its other devices, and this device forgets it.
    func resetPresenceMode() {
        presenceModeSync = PresenceModeSync(generation: presenceModeSync.generation + 1)
        status.mode = .automatic
        status.availability = .available
        sentPresence = nil
    }
}
