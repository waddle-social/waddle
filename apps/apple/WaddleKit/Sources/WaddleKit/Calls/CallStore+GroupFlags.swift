extension CallStore {
    /// Flip optimistically; a failed send reverts only this call's latest hand choice.
    public func setHandRaised(_ raised: Bool) async -> Bool {
        guard case .inGroup(let target) = state else { return false }
        let epoch = generation
        groupControl.handRevision += 1
        let revision = groupControl.handRevision
        let previous = isHandRaised
        setGroupMarkers(hand: raised)
        let sent = await restampGroupPresence(target, epoch: epoch)
        guard generation == epoch, let sent else { return false }
        if !sent, case .inGroup(let current) = state, current.sid == target.sid, current.room == target.room {
            if groupControl.handRevision == revision { setGroupMarkers(hand: previous) }
            reportError("muji hand-raise update failed", epoch: epoch)
        }
        return sent
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

    private func restampGroupPresence(_ target: GroupCall, epoch: Int) async -> Bool? {
        await groupControl.serializePresence { [self] in
            guard generation == epoch, case .inGroup(let current) = state,
                current.sid == target.sid, current.room == target.room else { return nil }
            // Snapshot both flags inside the queue, after the exact slot check.
            return await port.updateGroupCallPresence(
                .init(state: .active(video: current.media.video), isHandRaised: isHandRaised, isMuted: isSelfMuteAdvertised),
                in: current.room, nick: current.nick)
        }
    }
}
