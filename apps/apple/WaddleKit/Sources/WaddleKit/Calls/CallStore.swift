import Foundation
import Observation

/// A MainActor call slot; inbound reductions run before ordered remote effects.
@MainActor @Observable public final class CallStore {
    public let groupPresence = GroupCallPresence()
    /// Our optimistic choice; room echoes never overwrite local intent.
    public private(set) var isHandRaised = false
    /// Capture owns mute state; a failed presence write leaves this choice intact.
    public private(set) var isSelfMuteAdvertised = false
    /// The mute state the room last heard from us. It trails
    /// `isSelfMuteAdvertised` while a send is on its way or has failed.
    public private(set) var confirmedSelfMute = false
    @ObservationIgnored let groupControl: GroupCallControl

    /// The one call slot; terminal states remain until dismissed or replaced.
    public private(set) var state: CallState = .idle {
        didSet {
            let cause = exitCause
            exitCause = nil
            if oldValue.isLive, oldValue.sid != state.sid || !state.isLive, let sid = oldValue.sid {
                markFinished(sid: sid)
                if let outcome = lifecycle.finish(sid: sid, from: oldValue, to: state, cause: cause, at: now()) {
                    report(outcome)
                }
            }
            if state.sid != oldValue.sid {
                switch state {
                case .incoming(let call): lifecycle.begin(sid: call.sid, kind: .direct)
                case .outgoing(let call): lifecycle.begin(sid: call.sid, kind: .direct)
                case .joiningGroup(let call): lifecycle.begin(sid: call.sid, kind: .group)
                case .idle, .active, .inGroup, .ended: break
                }
            }
            if case .ended(let sid, _) = state { markFinished(sid: sid) }
            mediaSession?.update(state)
        }
    }
    /// Last failed wire verb; fresh calls, inbound transitions, dismissal and clear reset it.
    public private(set) var lastError: String?
    @ObservationIgnored private var lifecycle = CallLifecycleTracker()
    @ObservationIgnored var exitCause: CallLifecycleTracker.Exit?
    @ObservationIgnored private let now: @MainActor () -> ContinuousClock.Instant
    @ObservationIgnored private let report: @MainActor (CallLifecycleReport) -> Void
    @ObservationIgnored private var mediaSession: CallSessionController?
    @ObservationIgnored let port: any CallPort
    @ObservationIgnored let ownJID: @MainActor () -> JID?
    @ObservationIgnored private let ownBareJID: BareJID
    @ObservationIgnored private let newSid: () -> String
    @ObservationIgnored private let ringTimeout: Duration
    @ObservationIgnored private let sessionTimeout: Duration
    @ObservationIgnored private let unansweredTimeout: Duration
    /// The answer on its way to the caller, and how it will turn out.
    @ObservationIgnored var acceptInFlight: (sid: String, outcome: GroupCallWaiter<Bool>)?
    @ObservationIgnored private var ringTimer: Task<Void, Never>?
    @ObservationIgnored private var sessionTimer: Task<Void, Never>?
    /// Abort history is bounded because reductions can outpace the effects queue.
    @ObservationIgnored private(set) var recentlyAbortedSids: [String] = []
    /// Rooms that must get no more group presence from us: one sent to a
    /// room that was deleted would create it again.
    @ObservationIgnored var sealedGroupRooms: Set<BareJID> = []
    @ObservationIgnored private var finishedSids: [String] = []
    @ObservationIgnored private var declinedBeforeProposalSids: [String] = []
    @ObservationIgnored private(set) var generation = 0
    @ObservationIgnored private var effectTask: Task<Void, Never>?
    @ObservationIgnored private var effects: [@MainActor () async -> Void] = []

    /// The port belongs to this signed-in session; clear fences every suspended continuation.
    public init(
        port: any CallPort, ownJID: @escaping @MainActor () -> JID?, ownBareJID: BareJID,
        media: (any CallMediaController)? = nil,
        ringTimeout: Duration = .seconds(45), sessionTimeout: Duration = .seconds(45),
        unansweredTimeout: Duration = .seconds(60),
        groupPreparingEchoTimeout: Duration = .seconds(2), groupOtherPreparingTimeout: Duration = .seconds(2),
        groupAcceptTimeout: Duration = .seconds(10),
        now: @escaping @MainActor () -> ContinuousClock.Instant = { ContinuousClock.now },
        report: @escaping @MainActor (CallLifecycleReport) -> Void = { _ in /* A store nobody reports for, as in most tests. */ },
        newSid: @escaping () -> String = { "c" + UUID().uuidString.replacingOccurrences(of: "-", with: "").prefix(16).lowercased() }
    ) {
        self.groupControl = GroupCallControl(
            mixer: BareJID(localpart: nil, domain: "calls." + ownBareJID.domain),
            echoTimeout: groupPreparingEchoTimeout, peersTimeout: groupOtherPreparingTimeout, acceptTimeout: groupAcceptTimeout)
        self.ringTimeout = ringTimeout
        self.sessionTimeout = sessionTimeout
        self.unansweredTimeout = unansweredTimeout
        self.port = port
        self.ownJID = ownJID
        self.ownBareJID = ownBareJID
        self.newSid = newSid
        self.now = now
        self.report = report
        if let media {
            self.mediaSession = CallSessionController(
                media: media, fetchExternalServices: { await port.fetchExternalServices() },
                mediaConnected: { [weak self] sid in
                    guard let self else { return }
                    self.lifecycle.mediaConnected(sid: sid, at: self.now())
                },
                phaseChanged: { [weak self] sid, phase in
                    switch phase {
                    case .reconnecting: self?.lifecycle.mediaReconnecting(sid: sid)
                    case .connected: self?.lifecycle.mediaRecovered(sid: sid)
                    case .idle, .connecting: break
                    }
                },
                hangUp: { [weak self] sid, reason, endedByServer in
                    await self?.hangUpActive(ifSid: sid, reason: reason, cause: endedByServer ? .endedByServer : .mediaFailure)
                })
        }
    }

    /// Reduce arrivals synchronously; only remote-originated effects enter the serial queue.
    public func handle(_ event: CallEvent) {
        if case .propose = event.kind, isFinished(sid: event.sid) {
            if let index = declinedBeforeProposalSids.firstIndex(of: event.sid) {
                declinedBeforeProposalSids.remove(at: index)
                // Only the stream proposal can supply the reject's destination.
                if event.from.bare != ownBareJID, event.from.resource != nil {
                    let epoch = generation
                    enqueue { [weak self] in
                        guard let self else { return }
                        _ = await self.send(epoch: epoch, error: "call reject failed") {
                            await self.port.sendCallReject(to: event.from, sid: event.sid, tieBreak: false)
                        }
                    }
                }
            }
            return
        }
        if fulfillGroupAccept(event) { return }
        if endGroupFromRemote(event) { return }
        let before = state
        switch event.kind {
        case .reject where before.sid != event.sid, .retract where before.sid != event.sid:
            if !recentlyAbortedSids.contains(event.sid) { recentlyAbortedSids.append(event.sid) }
            if recentlyAbortedSids.count > 256 { recentlyAbortedSids.removeFirst() }
        default: break
        }
        let isSelfOriginated = event.from.bare == ownBareJID
        let next = reduceCallState(before, event, isOwn: isSelfOriginated)
        if next != before {
            exitCause = .remote(own: isSelfOriginated)
            transition(next)
        }
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
                guard let self, self.isRinging(sid: event.sid) else { return }
                _ = await self.send(epoch: epoch, error: "call ringing failed") {
                    await self.port.sendCallRinging(to: event.from.bare, sid: event.sid)
                }
            }
        }
        enqueue { [weak self] in
            await self?.remoteEffect(event, before: before, epoch: epoch)
        }
    }

    /// Remembers a call ended on this device, even before it held the slot.
    /// The last 32 distinct session ids survive dismissal and session clear.
    public func markFinished(sid: String) {
        guard !finishedSids.contains(sid) else { return }
        finishedSids.append(sid)
        if finishedSids.count > 32 { finishedSids.removeFirst() }
    }

    public func isFinished(sid: String) -> Bool { finishedSids.contains(sid) }

    /// An early decline waits for the stream proposal to address its reject.
    /// Like finished calls, the last 32 distinct declines survive session clear.
    /// An adopted call uses `end(sid:)` instead.
    public func declinePushedCall(sid: String) -> Bool {
        guard state.sid != sid || !state.isLive else { return false }
        markFinished(sid: sid)
        if let outcome = lifecycle.declinedUnseen(sid: sid) { report(outcome) }
        if !declinedBeforeProposalSids.contains(sid) {
            declinedBeforeProposalSids.append(sid)
            if declinedBeforeProposalSids.count > 32 { declinedBeforeProposalSids.removeFirst() }
        }
        return true
    }

    private func isRinging(sid: String) -> Bool {
        guard case .incoming(let call) = state else { return false }
        return call.sid == sid && !call.isAccepting && !isFinished(sid: sid)
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
            if case .outgoing(let current) = state, current.sid == sid { state = .ended(sid: sid, reason: .error) }
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
        // Whoever else asks to answer this call meanwhile shares the outcome.
        let outcome = GroupCallWaiter<Bool>()
        acceptInFlight = (target.sid, outcome)
        let sent = await port.sendCallProceed(to: target.from, sid: target.sid)
        if acceptInFlight?.outcome === outcome { acceptInFlight = nil }
        outcome.resolve(sent && generation == epoch)
        guard generation == epoch else { return false }
        if !sent {
            if case .incoming(var current) = state, current.sid == target.sid {
                current.isAccepting = false
                state = .incoming(current)
                // The ring is back, and its first timer may have come due
                // while the answer was on its way.
                scheduleUnansweredTimeout(sid: current.sid)
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
        exitCause = .decline
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
        sealedGroupRooms.removeAll()
        acceptInFlight?.outcome.cancel()
        acceptInFlight = nil
        generation += 1
        groupControl.clear()
        groupPresence.clear()
        setGroupMarkers(hand: false, muted: false, confirmedMute: false)
        effectTask?.cancel()
        effectTask = nil
        effects.removeAll()
        exitCause = .cleared
        state = .idle
        lastError = nil
    }

    func nextGroupSid() -> String { newSid() }

    func setGroupMarkers(hand: Bool? = nil, muted: Bool? = nil, confirmedMute: Bool? = nil) {
        if let hand { isHandRaised = hand }
        if let muted { isSelfMuteAdvertised = muted }
        if let confirmedMute, confirmedSelfMute != confirmedMute { confirmedSelfMute = confirmedMute }
    }

    /// Local teardown claims the slot without erasing the last wire failure.
    func claimHangUp() -> CallState {
        let claimed = state
        cancelTimers()
        exitCause = .hangUp
        state = .idle
        return claimed
    }

    func transition(_ next: CallState) {
        if state != next {
            if (state.isOutgoing && !next.isOutgoing) || (state.isIncoming && !next.isIncoming) {
                cancelTimers()
            }
            lastError = nil
            let previous = state
            state = next
            // Every way a call starts ringing here bounds the ring: a
            // proposal, a tie-break won, a slot taken over.
            if case .incoming(let call) = next, !call.isAccepting, !(previous.isIncoming && previous.sid == call.sid) {
                scheduleUnansweredTimeout(sid: call.sid)
            }
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

    /// A caller who lost their connection never retracts, so a call nobody
    /// answers ends here by itself, a little after the caller's own ring
    /// would have run out. Nobody is told: there is nobody left to tell.
    /// An accepted call is bounded by the session timeout instead.
    func scheduleUnansweredTimeout(sid: String) {
        ringTimer?.cancel()
        let epoch = generation
        ringTimer = Task { [weak self, unansweredTimeout] in
            do { try await Task.sleep(for: unansweredTimeout) } catch { return }
            guard !Task.isCancelled, let self, self.generation == epoch,
                case .incoming(let current) = self.state, current.sid == sid, !current.isAccepting
            else { return }
            self.state = .ended(sid: sid, reason: .missed)
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
