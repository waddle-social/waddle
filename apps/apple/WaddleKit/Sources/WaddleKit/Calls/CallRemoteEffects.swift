extension CallStore {
    /// Sends keep arrival order, but the reducer may already have advanced the slot.
    func remoteEffect(_ event: CallEvent, before: CallState, epoch: Int) async {
        guard generation == epoch else { return }
        if before.isGroupPhase, await endGroupFromRemote(event, epoch: epoch) { return }
        switch event.kind {
        case .propose(let media):
            // A redelivered proposal for the slot's own session needs no answer.
            guard before.sid != event.sid, !recentlyAbortedSids.contains(event.sid) else { return }
            if case .outgoing(let call) = before, call.to == event.from.bare {
                await tieBreak(event, media: media, previous: call, epoch: epoch)
            } else if case .active(let call) = before, call.peer.bare == event.from.bare {
                await migrate(event, media: media, previous: call, epoch: epoch)
            } else if before.isLive {
                _ = await send(epoch: epoch, error: "call reject failed") {
                    await port.sendCallReject(to: event.from, sid: event.sid, tieBreak: false)
                }
            }

        case .proceed:
            // A second device's proceed must not open a second session.
            guard case .outgoing(let call) = before, call.sid == event.sid, call.responder == nil,
                call.to == event.from.bare, callStillLive(event.sid)
            else { return }
            guard let own = ownJID() else {
                failCall(sid: event.sid, message: "call session initiate failed: no bound resource", epoch: epoch)
                return
            }
            let sent = await port.sendCallSessionInitiate(to: event.from, initiator: own, sid: event.sid, media: call.media)
            guard generation == epoch else { return }
            if sent {
                scheduleSessionTimeout(peer: event.from, sid: event.sid)
            } else {
                failCall(sid: event.sid, message: "call session initiate failed", epoch: epoch)
            }
        case .sessionInitiate(_, let media):
            guard case .incoming(let call) = before, call.sid == event.sid, call.isAccepting, call.from == event.from,
                callStillLive(event.sid)
            else { return }
            guard let own = ownJID() else {
                failCall(sid: event.sid, message: "call session accept failed: no bound resource", epoch: epoch)
                return
            }
            let sent = await port.sendCallSessionAccept(
                to: event.from, responder: own, sid: event.sid, media: media.bounded(by: call.media))
            guard generation == epoch else { return }
            if !sent { failCall(sid: event.sid, message: "call session accept failed", epoch: epoch) }
        case .finish, .sessionTerminate:
            // XEP-0353 has both parties send `<finish/>`. Ours answers the
            // first end event for a session we were in, so our other devices
            // and the archive learn it ended even if the peer drops offline.
            guard case .active(let call) = before, call.sid == event.sid, call.peer == event.from else { return }
            _ = await send(epoch: epoch, error: "call finish failed") {
                await port.sendCallFinish(to: call.peer, sid: call.sid, reason: .success)
            }
        default: break
        }
    }

    func callStillLive(_ sid: String) -> Bool { state.sid == sid && state.isLive }
}
