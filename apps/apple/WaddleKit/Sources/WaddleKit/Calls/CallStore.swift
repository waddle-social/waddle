import Foundation
import Observation

/// A MainActor call slot; inbound reductions run before ordered remote effects.
@MainActor @Observable public final class CallStore {
    /// The one call slot; terminal states remain until dismissed or replaced.
    public private(set) var state: CallState = .idle {
        didSet { mediaSession?.update(state) }
    }
    /// Last failed wire verb; fresh calls, inbound transitions, dismissal and clear reset it.
    public private(set) var lastError: String?
    @ObservationIgnored private var mediaSession: CallSessionController?
    @ObservationIgnored let port: any CallPort
    @ObservationIgnored let ownJID: @MainActor () -> JID?
    @ObservationIgnored private let ownBareJID: BareJID
    @ObservationIgnored private let newSid: () -> String
    @ObservationIgnored private let ringTimeout: Duration
    @ObservationIgnored private let sessionTimeout: Duration
    @ObservationIgnored private var ringTimer: Task<Void, Never>?
    @ObservationIgnored private var sessionTimer: Task<Void, Never>?
    /// Abort history is bounded because reductions can outpace the effects queue.
    @ObservationIgnored private(set) var recentlyAbortedSids: [String] = []
    @ObservationIgnored private(set) var generation = 0
    @ObservationIgnored private var effectTask: Task<Void, Never>?
    @ObservationIgnored private var effects: [@MainActor () async -> Void] = []

    /// The port belongs to this signed-in session; clear fences every suspended continuation.
    public init(
        port: any CallPort, ownJID: @escaping @MainActor () -> JID?, ownBareJID: BareJID,
        media: (any CallMediaController)? = nil,
        ringTimeout: Duration = .seconds(45), sessionTimeout: Duration = .seconds(45),
        newSid: @escaping () -> String = { "c" + UUID().uuidString.replacingOccurrences(of: "-", with: "").prefix(16).lowercased() }
    ) {
        self.ringTimeout = ringTimeout
        self.sessionTimeout = sessionTimeout
        self.port = port
        self.ownJID = ownJID
        self.ownBareJID = ownBareJID
        self.newSid = newSid
        if let media {
            self.mediaSession = CallSessionController(media: media) { [weak self] sid, reason in
                await self?.hangUpActive(ifSid: sid, reason: reason)
            }
        }
    }

    /// Reduce arrivals synchronously; only remote-originated effects enter the serial queue.
    public func handle(_ event: CallEvent) {
        let before = state
        switch event.kind {
        case .reject where before.sid != event.sid, .retract where before.sid != event.sid:
            if !recentlyAbortedSids.contains(event.sid) { recentlyAbortedSids.append(event.sid) }
            if recentlyAbortedSids.count > 256 { recentlyAbortedSids.removeFirst() }
        default: break
        }
        let isSelfOriginated = event.from.bare == ownBareJID
        transition(reduceCallState(before, event, isOwn: isSelfOriginated))
        // The ring is answered only when the reducer took this proceed's
        // sender as the responder; an ignored proceed leaves the timeout running.
        if case .outgoing(let was) = before, was.responder == nil, case .outgoing(let now) = state, now.responder != nil {
            ringTimer?.cancel()
            ringTimer = nil
        }
        guard !isSelfOriginated else { return }
        let epoch = generation
        if case .propose = event.kind, case .incoming(let call) = state, call.sid == event.sid,
            !(before.isIncoming && before.sid == event.sid)
        {
            enqueue { [weak self] in
                // A decline that ran while this effect was queued already answered the proposer.
                guard let self, self.state.isIncoming, self.state.sid == event.sid else { return }
                _ = await self.send(epoch: epoch, error: "call ringing failed") {
                    await self.port.sendCallRinging(to: event.from.bare, sid: event.sid)
                }
            }
        }
        enqueue { [weak self] in
            await self?.remoteEffect(event, before: before, epoch: epoch)
        }
    }

    /// Claim the one slot before the propose send can suspend.
    public func startCall(to peer: BareJID, media: CallMedia) async -> Bool {
        guard !state.isLive else { return false }
        cancelTimers()
        let sid = newSid()
        state = .outgoing(OutgoingCall(to: peer, sid: sid, media: media, initiator: ownJID()))
        lastError = nil
        let epoch = generation
        let sent = await port.sendCallPropose(to: peer, sid: sid, media: media)
        guard generation == epoch else { return false }
        if !sent {
            if case .outgoing(let current) = state, current.sid == sid { state = .idle }
            lastError = "call propose failed"
        }
        if sent { scheduleRingTimeout(sid: sid) }
        return sent
    }

    /// Proceed is claimed before sending so a second Accept cannot answer twice.
    public func acceptIncoming() async -> Bool {
        guard case .incoming(var target) = state, !target.isAccepting else { return false }
        target.isAccepting = true
        state = .incoming(target)
        let epoch = generation
        let sent = await port.sendCallProceed(to: target.from, sid: target.sid)
        guard generation == epoch else { return false }
        if !sent {
            if case .incoming(var current) = state, current.sid == target.sid {
                current.isAccepting = false
                state = .incoming(current)
            }
            lastError = "call proceed failed"
        }
        if sent { scheduleIncomingTimeout(peer: target.from, sid: target.sid) }
        return sent
    }

    /// An unanswered invitation is rejected to the proposer's full resource.
    public func declineIncoming() async -> Bool {
        guard case .incoming(let target) = state else { return false }
        cancelTimers()
        state = .idle
        return await answerDeclined(target)
    }

    /// Clearing a session fences all suspended sends as well as queued effects.
    public func clear() {
        ringTimer?.cancel()
        ringTimer = nil
        sessionTimer?.cancel()
        sessionTimer = nil
        recentlyAbortedSids.removeAll()
        generation += 1
        effectTask?.cancel()
        effectTask = nil
        effects.removeAll()
        state = .idle
        lastError = nil
    }

    /// Local teardown claims the slot without erasing the last wire failure.
    func claimHangUp() -> CallState {
        let claimed = state
        cancelTimers()
        state = .idle
        return claimed
    }

    func transition(_ next: CallState) {
        if state != next {
            if (state.isOutgoing && !next.isOutgoing) || (state.isIncoming && !next.isIncoming) {
                cancelTimers()
            }
            lastError = nil
            state = next
        }
    }

    /// Every suspension is fenced by the signed-in store's clear generation.
    func send(epoch: Int, error: String, _ operation: () async -> Bool) async -> Bool {
        guard generation == epoch else { return false }
        let sent = await operation()
        guard generation == epoch else { return false }
        if !sent { lastError = error }
        return sent
    }

    func failCall(sid: String, message: String, epoch: Int) {
        guard generation == epoch, state.sid == sid, state.isLive else { return }
        cancelTimers()
        transition(.ended(sid: sid, reason: .error))
        lastError = message
    }

    func reportError(_ message: String, epoch: Int) {
        guard generation == epoch else { return }
        lastError = message
    }

    func cancelTimers() {
        ringTimer?.cancel()
        ringTimer = nil
        sessionTimer?.cancel()
        sessionTimer = nil
    }

    func scheduleRingTimeout(sid: String) {
        // An answered ring is bounded by the session timeout instead.
        guard case .outgoing(let current) = state, current.sid == sid, current.responder == nil else { return }
        ringTimer?.cancel()
        let epoch = generation
        ringTimer = Task { [weak self, ringTimeout] in
            do { try await Task.sleep(for: ringTimeout) } catch { return }
            // A sleep whose deadline already passed returns normally even when cancelled.
            guard !Task.isCancelled, let self, self.generation == epoch,
                case .outgoing(let current) = self.state, current.sid == sid, current.responder == nil
            else { return }
            self.state = .ended(sid: sid, reason: .timeout)
            let sent = await self.port.sendCallRetract(to: current.to, sid: sid)
            guard self.generation == epoch else { return }
            if !sent { self.lastError = "call retract failed" }
        }
    }

    func scheduleSessionTimeout(peer: JID, sid: String) {
        guard case .outgoing(let current) = state, current.sid == sid else { return }
        sessionTimer?.cancel()
        let epoch = generation
        sessionTimer = Task { [weak self, sessionTimeout] in
            do { try await Task.sleep(for: sessionTimeout) } catch { return }
            guard !Task.isCancelled, let self, self.generation == epoch,
                case .outgoing(let current) = self.state, current.sid == sid
            else { return }
            self.state = .ended(sid: sid, reason: .timeout)
            let outcome = await self.port.sendCallSessionTerminate(to: peer, sid: sid, reason: .timeout)
            guard self.generation == epoch else { return }
            if outcome == .error { self.lastError = "call session terminate failed" }
            // The terminate is an IQ only the answering device sees; the
            // finish tells the peer's other devices the call is over.
            _ = await self.send(epoch: epoch, error: "call finish failed") {
                await self.port.sendCallFinish(to: peer, sid: sid, reason: .timeout)
            }
        }
    }

    func scheduleIncomingTimeout(peer: JID, sid: String) {
        guard state.isIncoming, state.sid == sid else { return }
        sessionTimer?.cancel()
        let epoch = generation
        sessionTimer = Task { [weak self, sessionTimeout] in
            do { try await Task.sleep(for: sessionTimeout) } catch { return }
            guard !Task.isCancelled, let self, self.generation == epoch, self.state.isIncoming, self.state.sid == sid else { return }
            self.state = .ended(sid: sid, reason: .timeout)
            _ = await self.send(epoch: epoch, error: "call finish failed") {
                await self.port.sendCallFinish(to: peer, sid: sid, reason: .timeout)
            }
        }
    }

    func enqueue(_ effect: @escaping @MainActor () async -> Void) {
        effects.append(effect)
        guard effectTask == nil else { return }
        let epoch = generation
        effectTask = Task { [weak self] in
            guard let self else { return }
            while self.generation == epoch, !self.effects.isEmpty {
                let next = self.effects.removeFirst()
                await next()
            }
            if self.generation == epoch { self.effectTask = nil }
        }
    }
}
