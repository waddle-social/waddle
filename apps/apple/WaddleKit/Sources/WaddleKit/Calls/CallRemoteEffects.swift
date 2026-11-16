extension CallStore {
    /// Sends keep arrival order, but the reducer may already have advanced the slot.
    func remoteEffect(_ event: CallEvent, before: CallState, epoch: Int) async {
        guard generation == epoch else { return }
        switch event.kind {
        case .propose(let media):
            guard !recentlyAbortedSids.contains(event.sid) else { return }
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
            guard case .incoming(let call) = before, call.sid == event.sid, call.isAccepting, callStillLive(event.sid)
            else { return }
            guard let own = ownJID() else {
                failCall(sid: event.sid, message: "call session accept failed: no bound resource", epoch: epoch)
                return
            }
            let sent = await port.sendCallSessionAccept(to: event.from, responder: own, sid: event.sid, media: media)
            guard generation == epoch else { return }
            if !sent { failCall(sid: event.sid, message: "call session accept failed", epoch: epoch) }
        default: break
        }
    }

    func callStillLive(_ sid: String) -> Bool { state.sid == sid && state.isLive }
}
