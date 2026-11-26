extension CallStore {
    /// Flip optimistically; a failed send reverts only this call's latest hand choice.
    public func setHandRaised(_ raised: Bool) async -> Bool {
        guard case .inGroup(let target) = state else { return false }
        let epoch = generation
        groupControl.handRevision += 1
        let revision = groupControl.handRevision
        setGroupMarkers(hand: raised)
        // Undone inside the queue, to what the room was last told: the next
        // presence must not carry a hand state nobody chose.
        let sent = await restampGroupPresence(target, epoch: epoch) { [self] in
            if groupControl.handRevision == revision { setGroupMarkers(hand: groupControl.advertisedHand) }
        }
        guard generation == epoch, let sent else { return false }
        if !sent, case .inGroup(let current) = state, current.sid == target.sid, current.room == target.room {
            reportError("muji hand-raise update failed", epoch: epoch)
        }
        return sent
    }

    /// The room needs telling that the microphone is `muted`: either it last
    /// heard otherwise, or the next presence we send would say otherwise.
    public func isSelfMuteOwed(_ muted: Bool) -> Bool {
        muted != confirmedSelfMute || muted != isSelfMuteAdvertised
    }

    /// Capture owns mute state, so failed broadcasts never roll it back.
    public func advertiseSelfMute(_ muted: Bool) async -> Bool {
        guard case .inGroup(let target) = state else { return false }
        let epoch = generation
        setGroupMarkers(muted: muted)
        let sent = await restampGroupPresence(target, epoch: epoch)
        guard generation == epoch, let sent else { return false }
        if !sent, case .inGroup(let current) = state, current.sid == target.sid, current.room == target.room {
            reportError("muji mute update failed", epoch: epoch)
        }
        return sent
    }

    /// Says again that we are in `room`'s call, with our hand and mute as
    /// they are, after we have come back into the room.
    ///
    /// A call outlives the stream it was set up on: media goes on while
    /// the connection is replaced. But coming back into the room is a new
    /// presence with no call in it, and the server forgets the old one's,
    /// so everyone else would see a call still going as one we had left
    /// (web `readvertiseMucCallPresence`).
    @discardableResult
    public func readvertiseGroupCall(in room: BareJID) async -> Bool {
        guard case .inGroup(let call) = state, call.room == room else { return false }
        return await restampGroupPresence(call, epoch: generation) ?? false
    }

    /// `onFailure` runs before the queue moves on to the next presence.
    private func restampGroupPresence(
        _ target: GroupCall, epoch: Int, onFailure: (@MainActor () -> Void)? = nil
    ) async -> Bool? {
        await groupControl.serializePresence { [self] in
            guard generation == epoch, case .inGroup(let current) = state,
                current.sid == target.sid, current.room == target.room else { return nil }
            // Snapshot both flags inside the queue, after the exact slot check.
            let hand = isHandRaised
            let muted = isSelfMuteAdvertised
            let sent = await sendGroupPresence(
                .init(state: .active(video: current.media.video), isHandRaised: hand, isMuted: muted),
                in: current.room, nick: current.nick)
            // Bookkeeping belongs to the call that sent: a send that returns
            // after the slot moved on must not touch the next call's.
            guard generation == epoch, case .inGroup(let now) = state,
                now.sid == target.sid, now.room == target.room else { return sent }
            if sent {
                groupControl.advertisedHand = hand
                setGroupMarkers(confirmedMute: muted)
            } else {
                onFailure?()
            }
            return sent
        }
    }
}
