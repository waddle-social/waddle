extension CallStore {
    /// Dismiss only terminal banners; live phases need a wire-answering action.
    public func dismiss() {
        guard case .ended = state else { return }
        cancelTimers()
        transition(.idle)
    }

    /// The media plane can end only the active sid it was connected to.
    public func hangUpActive(ifSid sid: String, reason: JingleReason) async {
        guard case .active(let current) = state, current.sid == sid else { return }
        await hangUp(reason: reason)
    }

    /// Claim idle before awaiting the wire, so a local hang-up wins queued effects.
    public func hangUp(reason: JingleReason = .success) async {
        let claimed = claimHangUp()
        let epoch = generation
        if case .incoming(let call) = claimed {
            let sent: Bool
            if call.isAccepting {
                sent = await port.sendCallFinish(to: call.from, sid: call.sid, reason: .cancel)
            } else {
                sent = await port.sendCallReject(to: call.from, sid: call.sid, tieBreak: false)
            }
            if !sent { reportError(call.isAccepting ? "call finish failed" : "call reject failed", epoch: epoch) }
        }
        if case .outgoing(let call) = claimed {
            let sent = await port.sendCallRetract(to: call.to, sid: call.sid)
            if !sent { reportError("call retract failed", epoch: epoch) }
        }
        if case .active(let call) = claimed {
            let outcome = await port.sendCallSessionTerminate(to: call.peer, sid: call.sid, reason: reason)
            guard generation == epoch else { return }
            if outcome == .error {
                reportError("call session terminate failed", epoch: epoch)
                return
            }
            let sent = await port.sendCallFinish(to: call.peer, sid: call.sid, reason: nil)
            if !sent { reportError("call finish failed", epoch: epoch) }
        }
    }
}
