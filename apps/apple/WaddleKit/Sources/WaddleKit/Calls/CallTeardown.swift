extension CallStore {
    /// Dismiss only terminal banners; live phases need a wire-answering action.
    public func dismiss() {
        guard case .ended = state else { return }
        cancelTimers()
        transition(.idle)
    }

    /// The media plane can end only the active sid it was connected to.
    ///
    /// Nobody chose this ending, so the slot keeps it on show as an ended
    /// call instead of going idle.
    public func hangUpActive(ifSid sid: String, reason: JingleReason) async {
        guard case .active(let current) = state, current.sid == sid else { return }
        cancelTimers()
        transition(.ended(sid: sid, reason: .finished(reason)))
        await terminate(current, reason: reason, epoch: generation)
    }

    /// Claim idle before awaiting the wire, so a local hang-up wins queued effects.
    public func hangUp(reason: JingleReason = .success) async {
        let claimed = claimHangUp()
        let epoch = generation
        if case .incoming(let call) = claimed { _ = await answerDeclined(call) }
        if case .outgoing(let call) = claimed {
            guard let responder = call.responder else {
                let sent = await port.sendCallRetract(to: call.to, sid: call.sid)
                if !sent { reportError("call retract failed", epoch: epoch) }
                return
            }
            // A device has answered and may already be in the session, where
            // a retract means nothing: end the Jingle session, then send the
            // finish. The finish goes out even when the server does not know
            // the session yet, since it is all a still-accepting device sees.
            let outcome = await port.sendCallSessionTerminate(to: responder, sid: call.sid, reason: reason)
            guard generation == epoch else { return }
            if outcome == .error { reportError("call session terminate failed", epoch: epoch) }
            let sent = await port.sendCallFinish(to: responder, sid: call.sid, reason: nil)
            if !sent { reportError("call finish failed", epoch: epoch) }
        }
        if case .active(let call) = claimed { await terminate(call, reason: reason, epoch: epoch) }
    }

    /// Ends the Jingle session, then sends the XEP-0353 finish.
    private func terminate(_ call: ActiveCall, reason: JingleReason, epoch: Int) async {
        let outcome = await port.sendCallSessionTerminate(to: call.peer, sid: call.sid, reason: reason)
        guard generation == epoch else { return }
        if outcome == .error {
            reportError("call session terminate failed", epoch: epoch)
            return
        }
        let sent = await port.sendCallFinish(to: call.peer, sid: call.sid, reason: nil)
        if !sent { reportError("call finish failed", epoch: epoch) }
    }

    /// Once our `<proceed/>` is out, the abandon verb is `<finish/>` with
    /// `<cancel/>`; a reject would contradict the proceed.
    func answerDeclined(_ call: IncomingCall) async -> Bool {
        let epoch = generation
        let sent: Bool
        if call.isAccepting {
            sent = await port.sendCallFinish(to: call.from, sid: call.sid, reason: .cancel)
        } else {
            sent = await port.sendCallReject(to: call.from, sid: call.sid, tieBreak: false)
        }
        guard generation == epoch else { return false }
        if !sent { reportError(call.isAccepting ? "call finish failed" : "call reject failed", epoch: epoch) }
        return sent
    }
}
