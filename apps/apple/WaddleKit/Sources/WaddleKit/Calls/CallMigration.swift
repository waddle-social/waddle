extension CallStore {
    /// XEP-0353 tie-break-2 retires the old JMI sid before proceeding the new one.
    func migrate(_ event: CallEvent, media: CallMedia, previous: ActiveCall, epoch: Int) async {
        guard slotStillMatches(.active(previous)) else {
            _ = await send(epoch: epoch, error: "call reject failed") {
                await port.sendCallReject(to: event.from, sid: event.sid, tieBreak: false)
            }
            return
        }
        let finished = await port.sendCallFinishMigrated(to: event.from, oldSid: previous.sid, newSid: event.sid)
        guard generation == epoch else { return }
        guard finished else {
            _ = await send(epoch: epoch, error: "call reject failed") {
                await port.sendCallReject(to: event.from, sid: event.sid, tieBreak: false)
            }
            failCall(sid: previous.sid, message: "call migration failed", epoch: epoch)
            return
        }
        // Migration is accepted without asking the user again, so it carries
        // only the tracks they agreed to for the session it replaces.
        let media = media.bounded(by: previous.media)
        // The new sid owns the slot before the proceed can suspend: the
        // caller's session-initiate may arrive while that send is in flight.
        var oldSessionIsOpen = true
        if !installIncoming(event, media: media, expected: .active(previous), accepting: true, epoch: epoch) {
            guard generation == epoch else { return }
            let proposalDied = recentlyAbortedSids.contains(event.sid)
            if proposalDied, slotStillMatches(.active(previous)) {
                // The old sid is already retired on the wire; it cannot stay active.
                cancelTimers()
                transition(.ended(sid: previous.sid, reason: .expired))
                retireOldSession(previous, epoch: epoch)
                return
            }
            guard !proposalDied, case .ended(sid: previous.sid, reason: _) = state else {
                // A local hang-up walked away before we answered: decline.
                _ = await send(epoch: epoch, error: "call reject failed") {
                    await port.sendCallReject(to: event.from, sid: event.sid, tieBreak: false)
                }
                return
            }
            // The old session ended remotely meanwhile; the re-proposal is still live.
            cancelTimers()
            transition(.incoming(IncomingCall(from: event.from, sid: event.sid, media: media, isAccepting: true)))
            oldSessionIsOpen = false
        }
        let proceeded = await port.sendCallProceed(to: event.from, sid: event.sid)
        guard generation == epoch else { return }
        if proceeded {
            scheduleIncomingTimeout(peer: event.from, sid: event.sid)
        } else if state.isIncoming, state.sid == event.sid {
            _ = await send(epoch: epoch, error: "call reject failed") {
                await port.sendCallReject(to: event.from, sid: event.sid, tieBreak: false)
            }
            failCall(sid: event.sid, message: "call migration failed", epoch: epoch)
        }
        if oldSessionIsOpen { retireOldSession(previous, epoch: epoch) }
    }

    /// An orphaned old IQ must not stall the serial invitation effects queue.
    func retireOldSession(_ call: ActiveCall, epoch: Int) {
        Task { [weak self] in
            guard let self, self.generation == epoch else { return }
            let outcome = await self.port.sendCallSessionTerminate(to: call.peer, sid: call.sid, reason: .expired)
            if outcome == .error { self.reportError("call session terminate failed", epoch: epoch) }
        }
    }
}
