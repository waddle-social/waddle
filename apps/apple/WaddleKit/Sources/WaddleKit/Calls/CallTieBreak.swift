extension CallStore {
    /// XEP-0353 tie-break-1 retracts the higher proposal before ringing the winner.
    func tieBreak(_ event: CallEvent, media: CallMedia, previous: OutgoingCall, epoch: Int) async {
        guard slotStillMatches(.outgoing(previous)) else {
            await answerAfterTieBreakSlotMoved(event, media: media, previous: previous, epoch: epoch)
            return
        }
        guard let own = ownJID() else {
            if compareOctetStrings(event.sid, previous.sid) >= 0 {
                _ = await send(epoch: epoch, error: "call tie-break reject failed") {
                    await port.sendCallReject(to: event.from, sid: event.sid, tieBreak: true)
                }
            } else {
                _ = await send(epoch: epoch, error: "call tie-break retract failed") {
                    await port.sendCallRetractTieBreak(to: event.from, sid: previous.sid)
                }
                _ = await send(epoch: epoch, error: "call reject failed") {
                    await port.sendCallReject(to: event.from, sid: event.sid, tieBreak: false)
                }
                guard generation == epoch else { return }
                endRetractedOutgoing(previous)
            }
            return
        }
        if incomingProposeWinsTieBreak(incomingSid: event.sid, outgoingSid: previous.sid, incomingFrom: event.from, ourFullJID: own) {
            let sent = await send(epoch: epoch, error: "call tie-break retract failed") {
                await port.sendCallRetractTieBreak(to: event.from, sid: previous.sid)
            }
            guard sent, generation == epoch else { return }
            guard installIncoming(event, media: media, expected: .outgoing(previous), epoch: epoch) else {
                endRetractedOutgoing(previous)
                await answerAfterTieBreakSlotMoved(event, media: media, previous: previous, epoch: epoch)
                return
            }
        } else {
            _ = await send(epoch: epoch, error: "call tie-break reject failed") {
                await port.sendCallReject(to: event.from, sid: event.sid, tieBreak: true)
            }
        }
    }

    /// The cause of a moved slot decides whether the winning proposal still rings.
    func answerAfterTieBreakSlotMoved(_ event: CallEvent, media: CallMedia, previous: OutgoingCall, epoch: Int) async {
        guard generation == epoch, !recentlyAbortedSids.contains(event.sid), !isFinished(sid: event.sid) else { return }
        if case .ended(sid: previous.sid, reason: .expired) = state {
            cancelTimers()
            transition(.incoming(IncomingCall(from: event.from, sid: event.sid, media: media)))
            guard await port.peerReceivesOurPresence(event.from.bare) else { return }
            _ = await send(epoch: epoch, error: "call ringing failed") {
                await port.sendCallRinging(to: event.from, sid: event.sid)
            }
        } else {
            _ = await send(epoch: epoch, error: "call reject failed") {
                await port.sendCallReject(to: event.from, sid: event.sid, tieBreak: false)
            }
        }
    }

    func endRetractedOutgoing(_ previous: OutgoingCall) {
        guard slotStillMatches(.outgoing(previous)) else { return }
        cancelTimers()
        transition(.ended(sid: previous.sid, reason: .expired))
    }

    func installIncoming(_ event: CallEvent, media: CallMedia, expected: CallState, accepting: Bool = false, epoch: Int) -> Bool {
        guard generation == epoch, slotStillMatches(expected), !recentlyAbortedSids.contains(event.sid), !isFinished(sid: event.sid) else { return false }
        cancelTimers()
        transition(.incoming(IncomingCall(from: event.from, sid: event.sid, media: media, isAccepting: accepting)))
        return true
    }

    /// Phase and sid identify the slot; ringing/accepting flag changes are benign.
    func slotStillMatches(_ expected: CallState) -> Bool {
        switch (state, expected) {
        case (.incoming(let current), .incoming(let previous)): current.sid == previous.sid
        case (.outgoing(let current), .outgoing(let previous)): current.sid == previous.sid
        case (.active(let current), .active(let previous)): current.sid == previous.sid
        // Group phases never enter 1:1 migration or tie-breaking.
        case (.joiningGroup, _), (.inGroup, _), (_, .joiningGroup), (_, .inGroup): false
        default: state == expected
        }
    }
}
