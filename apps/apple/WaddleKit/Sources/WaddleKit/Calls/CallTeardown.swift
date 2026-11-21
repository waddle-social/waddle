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
    public func hangUpActive(ifSid sid: String, reason: JingleReason, cause: CallLifecycleTracker.Exit = .remote(own: false)) async {
        if case .inGroup(let current) = state, current.sid == sid {
            let epoch = generation
            cancelTimers()
            exitCause = cause
            transition(.ended(sid: sid, reason: .finished(reason)))
            await teardownGroup(.inGroup(current), identity: ownJID(), epoch: epoch)
            return
        }
        guard case .active(let current) = state, current.sid == sid else { return }
        cancelTimers()
        exitCause = cause
        transition(.ended(sid: sid, reason: .finished(reason)))
        await terminate(current, reason: reason, epoch: generation)
    }

    /// Claim idle before awaiting the wire, so a local hang-up wins queued effects.
    public func hangUp(reason: JingleReason = .success) async {
        let claimed = claimHangUp()
        let epoch = generation
        if claimed.isGroupPhase {
            await teardownGroup(claimed, identity: ownJID(), epoch: epoch)
            return
        }
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

/// Answer and End from outside the dock (the system's call screen, a
/// notification) name the call they were shown for.
extension CallStore {
    /// Answers the ringing call `sid`. True when its answer has gone out,
    /// by this or by an earlier answer; false when that call is not
    /// ringing here or the answer could not be sent.
    public func answer(sid: String) async -> Bool {
        switch state {
        case .incoming(let call) where call.sid == sid:
            guard call.isAccepting else { return await acceptIncoming() }
            // Someone already answered it here. If that answer is still
            // on its way, it is not answered until it has gone out.
            guard let pending = acceptInFlight, pending.sid == sid else { return true }
            return await pending.outcome.wait(timeout: .seconds(60)) ?? false
        case .active(let call) where call.sid == sid:
            return true
        case .idle, .incoming, .outgoing, .active, .joiningGroup, .inGroup, .ended:
            return false
        }
    }

    /// Ends the call `sid` if the slot still holds it live: a ringing call
    /// is declined, any other is hung up.
    public func end(sid: String) async {
        guard state.sid == sid, state.isLive else { return }
        if case .incoming(let call) = state, !call.isAccepting {
            _ = await declineIncoming()
        } else {
            await hangUp()
        }
    }
}
