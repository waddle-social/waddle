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
        var proceeded = false
        if finished { proceeded = await port.sendCallProceed(to: event.from, sid: event.sid) }
        guard generation == epoch else { return }
        guard proceeded else {
            _ = await send(epoch: epoch, error: "call reject failed") {
                await port.sendCallReject(to: event.from, sid: event.sid, tieBreak: false)
            }
            failCall(sid: previous.sid, message: "call migration failed", epoch: epoch)
            return
        }
        guard installIncoming(event, media: media, expected: .active(previous), accepting: true, epoch: epoch) else {
            await abandonRefusedMigration(event, media: media, previous: previous, epoch: epoch)
            return
        }
        scheduleIncomingTimeout(peer: event.from, sid: event.sid)
        retireOldSession(previous, epoch: epoch)
    }

    /// A retracted re-proposal cannot strand the retired old session as active.
    func abandonRefusedMigration(_ event: CallEvent, media: CallMedia, previous: ActiveCall, epoch: Int) async {
        guard generation == epoch else { return }
        if recentlyAbortedSids.contains(event.sid), slotStillMatches(.active(previous)) {
            cancelTimers()
            transition(.ended(sid: previous.sid, reason: .expired))
            retireOldSession(previous, epoch: epoch)
        } else if !recentlyAbortedSids.contains(event.sid), case .ended(sid: previous.sid, reason: _) = state {
            cancelTimers()
            transition(.incoming(IncomingCall(from: event.from, sid: event.sid, media: media, isAccepting: true)))
            scheduleIncomingTimeout(peer: event.from, sid: event.sid)
        } else {
            // Our proceed is already on the wire; walking away requires its finish bookend.
            _ = await send(epoch: epoch, error: "call finish failed") {
                await port.sendCallFinish(to: event.from, sid: event.sid, reason: .cancel)
            }
        }
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
