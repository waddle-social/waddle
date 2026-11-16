/// Stale session and acknowledgement events never replace another sid's slot.
func reduceCallState(_ current: CallState, _ event: CallEvent) -> CallState {
    switch event.kind {
    case .propose(let media):
        if !current.isLive { return .incoming(IncomingCall(from: event.from, sid: event.sid, media: media)) }
    case .ringing:
        if case .outgoing(var call) = current, call.sid == event.sid, call.to == event.from.bare {
            call.isRinging = true
            return .outgoing(call)
        }
    case .proceed:
        if current.isIncoming, current.sid == event.sid { return .idle }
    case .reject(let reason, let tieBreak):
        if current.sid == event.sid { return .ended(sid: event.sid, reason: tieBreak && reason == .expired ? .expired : .rejected) }
    case .retract(let reason, let tieBreak):
        if current.sid == event.sid { return .ended(sid: event.sid, reason: tieBreak && reason == .expired ? .expired : .retracted) }
    case .sessionInitiate(let join, let media):
        if current.isIncoming, current.sid == event.sid {
            return .active(ActiveCall(peer: event.from, sid: event.sid, media: media, join: join, initiator: event.from))
        }
    case .sessionAccept(let join, let media):
        if case .outgoing(let call) = current, call.sid == event.sid {
            return .active(ActiveCall(peer: event.from, sid: event.sid, media: media, join: join, initiator: call.initiator))
        }
    case .finish(let reason, _), .sessionTerminate(let reason):
        if current.sid == event.sid { return .ended(sid: event.sid, reason: .finished(reason)) }
    }
    return current
}

/// Only sibling-device answered, declined, and ended transitions touch the slot.
func selfOriginatedEventShouldTouchCurrentCall(_ current: CallState, _ event: CallEvent) -> Bool {
    guard current.sid == event.sid else { return false }
    switch event.kind {
    case .propose, .ringing, .retract: return false
    case .proceed: return true
    case .reject, .sessionInitiate: return current.isIncoming
    case .sessionAccept: return current.isOutgoing
    case .finish, .sessionTerminate:
        if case .active = current { return true }
        return false
    }
}
