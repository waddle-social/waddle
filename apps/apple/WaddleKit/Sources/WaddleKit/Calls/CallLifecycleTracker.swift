/// Observes slot ownership and media without owning signaling or a clock.
public struct CallLifecycleTracker: Sendable {
    public enum Exit: Hashable, Sendable {
        case hangUp, decline, cleared, mediaFailure, remote(own: Bool)
        /// The SFU removed this device or closed the room: the other side
        /// ended the call, whichever signal arrived first.
        case endedByServer
    }

    private struct Attempt: Sendable {
        let kind: CallLifecycleReport.Kind
        var connectedAt: ContinuousClock.Instant?
        var reconnects = 0
        var isReconnecting = false
    }
    private var attempts: [String: Attempt] = [:]
    private var reported: [String] = []

    public init() {}

    public mutating func begin(sid: String, kind: CallLifecycleReport.Kind) {
        guard !sid.isEmpty, attempts[sid] == nil, !reported.contains(sid) else { return }
        attempts[sid] = Attempt(kind: kind)
    }

    public mutating func mediaConnected(sid: String, at instant: ContinuousClock.Instant) {
        guard var attempt = attempts[sid] else { return }
        if attempt.connectedAt == nil { attempt.connectedAt = instant }
        attempt.isReconnecting = false
        attempts[sid] = attempt
    }

    public mutating func mediaReconnecting(sid: String) {
        guard var attempt = attempts[sid] else { return }
        attempt.reconnects += 1
        attempt.isReconnecting = true
        attempts[sid] = attempt
    }

    public mutating func mediaRecovered(sid: String) {
        attempts[sid]?.isReconnecting = false
    }

    public mutating func finish(
        sid: String, from: CallState, to: CallState, cause: Exit?, at instant: ContinuousClock.Instant
    ) -> CallLifecycleReport? {
        guard let attempt = attempts.removeValue(forKey: sid), !reported.contains(sid) else { return nil }
        remember(sid)
        let outcome = Self.outcome(from: from, to: to, cause: cause, connected: attempt.connectedAt != nil, reconnecting: attempt.isReconnecting)
        return CallLifecycleReport(
            kind: attempt.kind, setupOutcome: attempt.connectedAt == nil ? outcome.0 : .accepted,
            endReason: outcome.1, duration: Self.duration(from: attempt.connectedAt, to: instant),
            reconnects: attempt.reconnects == 0 ? .none : attempt.reconnects == 1 ? .once : .multiple)
    }

    public mutating func declinedUnseen(sid: String) -> CallLifecycleReport? {
        guard attempts[sid] == nil, !reported.contains(sid) else { return nil }
        remember(sid)
        return CallLifecycleReport(kind: .direct, setupOutcome: .declined, endReason: .hangup, duration: .none, reconnects: .none)
    }

    private mutating func remember(_ sid: String) {
        reported.append(sid)
        if reported.count > 256 { reported.removeFirst() }
    }

    private static func duration(from start: ContinuousClock.Instant?, to end: ContinuousClock.Instant) -> CallLifecycleReport.DurationBucket {
        guard let start else { return .none }
        let elapsed = end - start
        if elapsed <= .zero { return .none }
        if elapsed < .seconds(60) { return .underOneMinute }
        if elapsed < .seconds(600) { return .oneToTenMinutes }
        if elapsed < .seconds(3600) { return .tenToSixtyMinutes }
        return .overSixtyMinutes
    }

    private static func outcome(
        from: CallState, to: CallState, cause: Exit?, connected: Bool, reconnecting: Bool
    ) -> (CallLifecycleReport.SetupOutcome, CallLifecycleReport.EndReason) {
        let incoming: Bool
        let joining: Bool
        switch from {
        case .incoming: (incoming, joining) = (true, false)
        case .joiningGroup: (incoming, joining) = (false, true)
        case .idle, .outgoing, .active, .inGroup, .ended: (incoming, joining) = (false, false)
        }
        let own: Bool
        switch cause {
        case .hangUp?, .cleared?: return (incoming ? .declined : .proposed, .hangup)
        case .decline?: return (.declined, .hangup)
        case .mediaFailure?: return (.failed, connected && reconnecting ? .reconnectExhausted : .error)
        case .endedByServer?: return (.proposed, .peerLeft)
        case .remote(let value)?: own = value
        case nil: own = false
        }
        switch to {
        case .idle:
            if incoming && own { return (.proposed, .hangup) }
            return joining ? (.failed, .error) : (.proposed, .hangup)
        case .incoming, .outgoing, .active, .joiningGroup, .inGroup:
            return (.proposed, connected ? .peerLeft : .hangup)
        case .ended(_, let reason):
            if joining { return (.failed, .error) }
            switch reason {
            case .rejected: return (.declined, own ? .hangup : .peerLeft)
            case .expired:
                // A sibling can expire an incoming slot only by rejecting its tie-break.
                if incoming && own { return (.declined, .hangup) }
                return (.proposed, connected && own ? .hangup : .peerLeft)
            case .retracted, .missed: return (.proposed, connected && own ? .hangup : .peerLeft)
            case .timeout: return (.timeout, .error)
            case .error: return (.failed, .error)
            case .finished(let reason):
                switch reason {
                case .decline?, .busy?: return (.declined, own ? .hangup : .peerLeft)
                case nil, .success?, .cancel?, .alternativeSession?, .expired?: return (.proposed, connected && own ? .hangup : .peerLeft)
                // The other device's timer or failure: that device reports
                // it. Counting it here too would double one incident, and
                // this device's media was healthy.
                case .timeout?: return (.timeout, connected ? .peerLeft : .error)
                case .connectivityError?, .failedApplication?, .failedTransport?, .generalError?, .gone?,
                    .incompatibleParameters?, .mediaError?, .securityError?, .unsupportedApplications?, .unsupportedTransports?:
                    return (.proposed, connected && own ? .hangup : .peerLeft)
                }
            }
        }
    }
}
