import Foundation

/// What is known of the account's stored presence mode (ADR-010).
struct PresenceModeSync {
    /// The mode the server is known to hold; nil until one is stored or
    /// read in this session.
    var stored: PresenceMode?
    /// The mode on its way to the server.
    var publishing: PresenceMode?
    /// A pick was made here while another was on its way.
    var needsAnotherPass = false
    /// Another device's pick that arrived while ours was on its way. The
    /// server heard theirs first, so it stands only if ours is not taken.
    var heardWhilePublishing: PresenceMode?
    /// Moves when `stored` is set by another device or forgotten on sign
    /// out, so a read or a publish that was under way then does not put
    /// an older value back.
    var generation = 0
}

/// When this device last saw input.
struct IdleClock {
    /// Nil until the first input or the first read, which starts it.
    var lastActive: Date?
    /// A picked status held auto-away off: idle time counts from when
    /// that ends, not from the input before it.
    var restartsWhenAutomatic = false

    /// Readings of the same input differ by a hair (a Mac reports its idle
    /// time, not the instant), so only a clearly later one is new input.
    static let sameInput: TimeInterval = 1
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
        status.presenceMode = mode
        await refreshOwnPresence()
        await storePresenceMode()
    }

    /// The RFC 6121 status message; blank removes it.
    public func setStatusText(_ text: String?) async {
        let trimmed = text?.trimmingCharacters(in: .whitespacesAndNewlines)
        status.statusText = (trimmed?.isEmpty ?? true) ? nil : trimmed
        await refreshOwnPresence()
    }

    // MARK: - Auto-away

    /// The user did something on this device at `instant` (now when nil).
    /// Reports may come late and out of order: only a later one counts.
    public func noteActivity(at instant: Date? = nil) {
        let instant = instant ?? now()
        if idleClock.lastActive.map({ instant > $0.addingTimeInterval(IdleClock.sameInput) }) ?? true {
            idleClock.lastActive = instant
        }
    }

    /// Works out what this device should show now and tells the server if
    /// that changed. The host calls it on a timer and after input.
    public func refreshOwnPresence() async {
        let presence = readIdleClock()
        status.availability = presence.availability
        let next = SentPresence(presence: presence, statusText: status.statusText)
        guard !isStopped, connection == .online, next != sentPresence else { return }
        sentPresence = next
        await port.sendPresence(presence.availability, status: next.statusText, idleSince: presence.idleSince)
    }

    /// What this device should show now. Reading moves the idle clock on
    /// where a picked status or a call stood in for input.
    private func readIdleClock() -> OwnPresence {
        let time = now()
        if case let .manual(picked) = status.presenceMode {
            idleClock.restartsWhenAutomatic = true
            return OwnPresence(availability: picked.availability, idleSince: nil)
        }
        var lastActive = idleClock.lastActive ?? time
        // Being in a call is proof of presence (ADR-010).
        if idleClock.restartsWhenAutomatic || calls.state.isEstablished {
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
        let mode = status.presenceMode
        if presenceModeSync.stored == mode { return }
        presenceModeSync.publishing = mode
        let generation = presenceModeSync.generation
        let serverTookIt = (try? await port.publishPresenceMode(mode)) != nil
        presenceModeSync.publishing = nil
        let heard = presenceModeSync.heardWhilePublishing
        presenceModeSync.heardWhilePublishing = nil
        // Signed out meanwhile: nothing here is this session's any more.
        guard presenceModeSync.generation == generation else { return }
        if serverTookIt {
            presenceModeSync.stored = mode
        } else if let heard {
            presenceModeSync.stored = heard
            if status.presenceMode == mode {
                status.presenceMode = heard
                await refreshOwnPresence()
            }
        }
        let again = presenceModeSync.needsAnotherPass || (serverTookIt && status.presenceMode != mode)
        presenceModeSync.needsAnotherPass = false
        if again {
            await storePresenceMode()
        }
    }

    /// A pick the server does not hold yet. Before anything is known of
    /// the server, only a picked status counts: a new device is Automatic
    /// by default and must not write that over the account's pick.
    private var hasUnstoredPick: Bool {
        guard let stored = presenceModeSync.stored else { return status.presenceMode != .automatic }
        return status.presenceMode != stored
    }

    /// On connect: a pick made offline wins and is stored; otherwise this
    /// device takes up what the account has stored. If that cannot be
    /// read, the device stays as it was.
    func syncPresenceModeOnConnect() async {
        if hasUnstoredPick {
            await storePresenceMode()
            return
        }
        let before = status.presenceMode
        let generation = presenceModeSync.generation
        let fetched = try? await port.fetchPresenceMode()
        guard !isStopped, !Task.isCancelled else { return }
        // A pick here or on another device arrived while reading: it is
        // newer than what was read.
        guard status.presenceMode == before, presenceModeSync.generation == generation else {
            await storePresenceMode()
            return
        }
        guard let fetched else { return }
        presenceModeSync.stored = fetched
        status.presenceMode = fetched
        await refreshOwnPresence()
    }

    /// Another device of the account picked `mode`. Taken up here without
    /// storing it again, or two devices would answer each other forever.
    func presenceModePicked(elsewhere mode: PresenceMode) {
        guard !isStopped else { return }
        guard presenceModeSync.publishing == nil else {
            presenceModeSync.heardWhilePublishing = mode
            return
        }
        presenceModeSync.generation += 1
        presenceModeSync.stored = mode
        guard status.presenceMode != mode else { return }
        status.presenceMode = mode
        Task { await self.refreshOwnPresence() }
    }

    /// Signing out is local: the account's pick stays on the server for
    /// its other devices, and this device forgets it.
    func resetPresenceMode() {
        presenceModeSync = PresenceModeSync(generation: presenceModeSync.generation + 1)
        status.presenceMode = .automatic
        status.availability = .available
        sentPresence = nil
    }
}
