/// An event changes the slot only when it names the slot's session, comes
/// from the party entitled to send it, and fits the slot's phase. Anything
/// else (a replay, a late answer from another device, a stranger who learned
/// the sid) leaves the slot as it is.
///
/// `isOwn` marks a carbon of something another of our own devices sent.
func reduceCallState(_ current: CallState, _ event: CallEvent, isOwn: Bool) -> CallState {
    guard !current.isGroupPhase else { return current }
    if case .propose(let media) = event.kind {
        // A redelivered proposal for the session the slot already holds,
        // live or ended, is not a new call.
        guard !isOwn, !current.isLive, current.sid != event.sid else { return current }
        return .incoming(IncomingCall(from: event.from, sid: event.sid, media: media))
    }
    guard current.sid == event.sid else { return current }
    return isOwn ? reduceSiblingEvent(current, event) : reducePeerEvent(current, event)
}

/// Another of our devices answered, declined or ended the call this device
/// is still showing.
private func reduceSiblingEvent(_ current: CallState, _ event: CallEvent) -> CallState {
    switch (event.kind, current) {
    case (.proceed, .incoming):
        return .idle
    case (.reject(let reason, let tieBreak), .incoming):
        return .ended(sid: event.sid, reason: abortReason(reason, tieBreak: tieBreak, otherwise: .rejected))
    case (.finish(let reason, _), .incoming), (.finish(let reason, _), .active):
        // A sibling that answered and hung up also ends a ring this device
        // kept, e.g. after it missed the sibling's proceed while offline.
        return .ended(sid: event.sid, reason: .finished(reason))
    case (.sessionTerminate(let reason), .active):
        return .ended(sid: event.sid, reason: .finished(reason))
    default:
        return current
    }
}

private func reducePeerEvent(_ current: CallState, _ event: CallEvent) -> CallState {
    switch current {
    case .incoming(let call):
        guard call.from.bare == event.from.bare else { return current }
        switch event.kind {
        case .retract(let reason, let tieBreak):
            return .ended(sid: call.sid, reason: abortReason(reason, tieBreak: tieBreak, otherwise: .retracted))
        case .sessionInitiate(let join, let media):
            // Media starts only for an invitation this device accepted, from
            // the device that proposed it, with no track beyond those agreed.
            guard call.isAccepting, call.from == event.from else { return current }
            return .active(
                ActiveCall(peer: event.from, sid: call.sid, media: media.bounded(by: call.media), join: join, initiator: event.from))
        case .finish(let reason, _), .sessionTerminate(let reason):
            return .ended(sid: call.sid, reason: .finished(reason))
        default:
            return current
        }

    case .outgoing(var call):
        // Once one of the peer's devices has answered, only that device
        // speaks for the call.
        guard call.to == event.from.bare, call.responder == nil || call.responder == event.from else { return current }
        switch event.kind {
        case .ringing:
            call.isRinging = true
            return .outgoing(call)
        case .proceed:
            guard call.responder == nil else { return current }
            call.responder = event.from
            return .outgoing(call)
        case .reject(let reason, let tieBreak):
            return .ended(sid: call.sid, reason: abortReason(reason, tieBreak: tieBreak, otherwise: .rejected))
        case .sessionAccept(let join, let media):
            return .active(
                ActiveCall(peer: event.from, sid: call.sid, media: media.bounded(by: call.media), join: join, initiator: call.initiator))
        case .finish(let reason, _), .sessionTerminate(let reason):
            return .ended(sid: call.sid, reason: .finished(reason))
        default:
            return current
        }

    case .active(let call):
        // An established session ends only on its own end events, from the
        // device it was established with.
        guard call.peer == event.from else { return current }
        switch event.kind {
        case .finish(let reason, _), .sessionTerminate(let reason):
            return .ended(sid: call.sid, reason: .finished(reason))
        default:
            return current
        }

    case .idle, .ended, .joiningGroup, .inGroup:
        return current
    }
}

/// XEP-0353 tie-breaking marks the losing proposal `<expired/>` with `<tie-break/>`.
private func abortReason(_ reason: JingleReason?, tieBreak: Bool, otherwise: CallEndReason) -> CallEndReason {
    tieBreak && reason == .expired ? .expired : otherwise
}
