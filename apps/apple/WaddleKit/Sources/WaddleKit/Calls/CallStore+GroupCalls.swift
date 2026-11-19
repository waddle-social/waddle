extension CallStore {
    /// Claim the shared slot before XEP-0272's two-phase room presence setup.
    public func joinGroupCall(in room: BareJID, media: CallMedia, nick: String) async -> Bool {
        guard !state.isLive else { return false }
        guard groupControl.mixer != nil else {
            reportError("muji session initiate failed: invalid mixer jid", epoch: generation)
            return false
        }
        guard !nick.isEmpty else {
            reportError("muc call needs a room jid and a nick", epoch: generation)
            return false
        }
        let attempt = GroupCallAttempt(room: room, sid: nextGroupSid(), media: media, nick: nick)
        let epoch = generation
        cancelTimers()
        transition(.joiningGroup(attempt))
        setGroupMarkers(hand: false, muted: !media.audio, confirmedMute: !media.audio)
        groupControl.advertisedHand = false
        let identity = ownJID()
        guard groupStillPending(attempt, epoch: epoch) else { return false }
        return await runGroupSetup(attempt, identity: identity, epoch: epoch)
    }

    /// Room presence is parsed at the bridge, including absence as a leave marker.
    ///
    /// This device is in a room's call only while the slot says so. The room
    /// can echo a presence we sent just before leaving after we have left,
    /// and that must not put us back in the call; another device of ours is
    /// someone else as far as this goes.
    public func handleGroupPresence(_ presence: WirePresence) {
        groupPresence.apply(presence, ignoringCallMarker: isLateOwnEcho(presence))
    }

    private func isLateOwnEcho(_ presence: WirePresence) -> Bool {
        guard presence.groupCall?.media != nil, !groupSlotHolds(presence.from.bare),
            let occupant = presence.occupant else { return false }
        return occupant.realJID.map { $0 == ownJID() } ?? occupant.statusCodes.contains(110)
    }

    private func runGroupSetup(_ attempt: GroupCallAttempt, identity: JID?, epoch: Int) async -> Bool {
        guard await prepareGroup(attempt, identity: identity, epoch: epoch) else {
            return await rollbackGroup(attempt, identity: identity, message: "muc call preparation failed", epoch: epoch)
        }
        let published = await groupControl.serializePresence { [self] in
            guard groupStillPending(attempt, epoch: epoch), !Task.isCancelled else { return nil }
            return await port.updateGroupCallPresence(
                .init(state: .active(video: attempt.media.video), isMuted: !attempt.media.audio), in: attempt.room, nick: attempt.nick)
        }
        guard generation == epoch else { return false }
        guard published == true else {
            return await rollbackGroup(attempt, identity: identity, message: "muji active presence failed", epoch: epoch)
        }
        guard groupStillPending(attempt, epoch: epoch), case .joiningGroup(var pending) = state else {
            await leaveGroupPresence(in: attempt.room, nick: attempt.nick, identity: identity, epoch: epoch)
            return false
        }
        pending.isAdvertised = true
        transition(.joiningGroup(pending))
        guard let initiator = ownJID() else {
            return await rollbackGroup(attempt, identity: identity, message: "muji session initiate failed: no bound resource", epoch: epoch)
        }
        // The injected resource lookup can synchronously clear or replace the slot.
        guard groupStillPending(attempt, epoch: epoch) else { return false }
        let waiter = GroupCallWaiter<CallJoin>()
        groupControl.abandonedAccepts.removeValue(forKey: attempt.sid)
        groupControl.accepts[attempt.sid] = .init(room: attempt.room, waiter: waiter)
        guard groupStillPending(attempt, epoch: epoch) else {
            cancelGroupAccept(attempt.sid)
            return false
        }
        guard !Task.isCancelled else {
            return await rollbackGroup(attempt, identity: identity, message: "muji session initiate failed", epoch: epoch)
        }
        let sent = await port.sendGroupCallSessionInitiate(
            in: attempt.room, initiator: initiator, sid: attempt.sid, video: attempt.media.video)
        guard generation == epoch else { return false }
        guard sent else {
            cancelGroupAccept(attempt.sid)
            return await rollbackGroup(attempt, identity: identity, message: "muji session initiate failed", epoch: epoch)
        }
        let join = await waiter.wait(timeout: groupControl.acceptTimeout)
        guard generation == epoch else { return false }
        cancelGroupAccept(attempt.sid)
        guard let join else {
            return await rollbackGroup(attempt, identity: identity, message: "muji session initiate failed", epoch: epoch)
        }
        if Task.isCancelled, groupStillPending(attempt, epoch: epoch) {
            return await rollbackGroup(attempt, identity: identity, message: "muji session initiate failed", epoch: epoch)
        }
        guard groupStillPending(attempt, epoch: epoch) else {
            // Accept won its resolver race, but teardown claimed the slot before promotion.
            await terminateAbandonedGroup(in: attempt.room, sid: attempt.sid, epoch: epoch)
            return false
        }
        transition(.inGroup(GroupCall(room: attempt.room, sid: attempt.sid, media: attempt.media, join: join, nick: attempt.nick)))
        return true
    }

    private func prepareGroup(_ attempt: GroupCallAttempt, identity: JID?, epoch: Int) async -> Bool {
        // Registration must precede even a synchronous room echo from the send.
        let waiter = groupPresence.registerPreparingEcho(in: attempt.room, nick: attempt.nick, realJID: identity)
        let sent = await groupControl.serializePresence { [self] in
            guard groupStillPending(attempt, epoch: epoch), !Task.isCancelled else { return nil }
            return await port.updateGroupCallPresence(.init(state: .preparing), in: attempt.room, nick: attempt.nick)
        }
        guard generation == epoch else { return false }
        guard sent == true else { return false }
        if groupStillPending(attempt, epoch: epoch), !Task.isCancelled,
            await groupPresence.awaitPreparingEcho(waiter, timeout: groupControl.echoTimeout),
            groupStillPending(attempt, epoch: epoch), !Task.isCancelled,
            await groupPresence.awaitNoOtherPreparing(
                in: attempt.room, nick: attempt.nick, realJID: identity, timeout: groupControl.peersTimeout),
            groupStillPending(attempt, epoch: epoch), !Task.isCancelled
        { return true }
        if !groupStillPending(attempt, epoch: epoch) {
            // An abandoned preparing send may have completed after teardown claimed it.
            await leaveGroupPresence(in: attempt.room, nick: attempt.nick, identity: identity, epoch: epoch)
        }
        return false
    }

    func groupStillPending(_ attempt: GroupCallAttempt, epoch: Int) -> Bool {
        guard generation == epoch, case .joiningGroup(let current) = state else { return false }
        return current.sid == attempt.sid && current.room == attempt.room
    }

    private func rollbackGroup(_ attempt: GroupCallAttempt, identity: JID?, message: String, epoch: Int) async -> Bool {
        guard groupStillPending(attempt, epoch: epoch), case .joiningGroup(let claimed) = state else { return false }
        // Nobody chose this ending, so it stays on show: tapping Join must
        // not look like nothing happened.
        cancelTimers()
        transition(.ended(sid: attempt.sid, reason: .error))
        await teardownGroup(.joiningGroup(claimed), identity: identity, epoch: epoch)
        reportError(message, epoch: epoch)
        return false
    }

    func groupSlotHolds(_ room: BareJID) -> Bool {
        switch state {
        case .joiningGroup(let call): call.room == room
        case .inGroup(let call): call.room == room
        case .idle, .incoming, .outgoing, .active, .ended: false
        }
    }

    func leaveGroupPresence(in room: BareJID, nick: String, identity: JID?, epoch: Int) async {
        _ = await groupControl.serializePresence { [self] in
            await leaveGroupPresenceNow(in: room, nick: nick, identity: identity, epoch: epoch)
            return true
        }
    }

    /// Runs inside the presence queue. Stands down when the slot holds a
    /// group phase for the room: a newer attempt owns its advertisement.
    private func leaveGroupPresenceNow(in room: BareJID, nick: String, identity: JID?, epoch: Int) async {
        guard generation == epoch, !groupSlotHolds(room) else { return }
        let cleared = await port.updateGroupCallPresence(.init(state: .left), in: room, nick: nick)
        guard generation == epoch else { return }
        if !cleared { reportError("muji leave presence failed", epoch: epoch) }
        // A successor may have claimed the room while the leave was in flight.
        if !groupSlotHolds(room) { groupPresence.clearParticipant(in: room, nick: nick, realJID: identity) }
        if !state.isGroupPhase { setGroupMarkers(hand: false, muted: false, confirmedMute: false) }
    }

    func terminateGroup(in room: BareJID, sid: String, epoch: Int) async {
        guard generation == epoch else { return }
        _ = await send(epoch: epoch, error: "muji session terminate failed") {
            await port.sendGroupCallSessionTerminate(in: room, sid: sid)
        }
    }

    /// The caller has already claimed the slot. The leave and the terminate
    /// go through the presence queue as one unit, leave first: a rejoin of
    /// the same room queues behind both, so the old session's terminate can
    /// never reach the mixer after the new attempt's advertisement.
    ///
    /// `cancelsWaits` is false for a cleanup that runs later than its claim:
    /// waits are found by room and nick, and by then they may belong to the
    /// next attempt in the same room.
    func teardownGroup(_ claimed: CallState, identity: JID?, epoch: Int, cancelsWaits: Bool = true) async {
        let room: BareJID
        let nick: String
        let sid: String
        let terminates: Bool
        switch claimed {
        case .joiningGroup(let call): (room, nick, sid, terminates) = (call.room, call.nick, call.sid, call.isAdvertised)
        case .inGroup(let call): (room, nick, sid, terminates) = (call.room, call.nick, call.sid, true)
        case .idle, .incoming, .outgoing, .active, .ended: return
        }
        if cancelsWaits { cancelGroupWaits(for: claimed) }
        forgetOwnGroupEntry(in: room, nick: nick, identity: identity, epoch: epoch)
        _ = await groupControl.serializePresence { [self] in
            await leaveGroupPresenceNow(in: room, nick: nick, identity: identity, epoch: epoch)
            if terminates { await terminateGroup(in: room, sid: sid, epoch: epoch) }
            return true
        }
    }

    /// We are out of the call as soon as the slot says so; the roster must
    /// not show us in it while the leave is still on its way to the room.
    /// Skipped when a newer attempt already holds the room.
    func forgetOwnGroupEntry(in room: BareJID, nick: String, identity: JID?, epoch: Int) {
        guard generation == epoch, !groupSlotHolds(room) else { return }
        groupPresence.clearParticipant(in: room, nick: nick, realJID: identity)
    }

    /// A claimed attempt's waits end at once, whoever claimed it.
    func cancelGroupWaits(for claimed: CallState) {
        switch claimed {
        case .joiningGroup(let call):
            groupPresence.cancelPreparationWaiters(in: call.room, nick: call.nick)
            cancelGroupAccept(call.sid)
        case .inGroup(let call):
            cancelGroupAccept(call.sid)
        case .idle, .incoming, .outgoing, .active, .ended: break
        }
    }
}

extension CallStore {
    /// Only the account's mixer can end a group sid; peer and sibling ends
    /// cannot. The slot ends as the event is handled, so a join in flight
    /// stops and media disconnects at once. The wire cleanup runs on its
    /// own, ordered by the presence queue, so it neither waits behind
    /// unrelated 1:1 sends nor holds them up.
    func endGroupFromRemote(_ event: CallEvent) -> Bool {
        guard state.isGroupPhase, state.sid == event.sid, event.from.bare == groupControl.mixer else { return false }
        let reason: CallEndReason
        switch event.kind {
        case .sessionTerminate(let condition), .finish(let condition, _): reason = .finished(condition)
        case .reject(let condition, let tieBreak): reason = tieBreak && condition == .expired ? .expired : .rejected
        case .retract(let condition, let tieBreak): reason = tieBreak && condition == .expired ? .expired : .retracted
        case .propose, .ringing, .proceed, .sessionInitiate, .sessionAccept: return false
        }
        let claimed = state
        let epoch = generation
        cancelGroupWaits(for: claimed)
        exitCause = .remote(own: false)
        transition(.ended(sid: event.sid, reason: reason))
        if case .inGroup(let call) = claimed {
            forgetOwnGroupEntry(in: call.room, nick: call.nick, identity: ownJID(), epoch: epoch)
        } else if case .joiningGroup(let call) = claimed {
            forgetOwnGroupEntry(in: call.room, nick: call.nick, identity: ownJID(), epoch: epoch)
        }
        Task { [weak self] in
            guard let self else { return }
            await self.teardownGroup(claimed, identity: self.ownJID(), epoch: epoch, cancelsWaits: false)
        }
        return true
    }
}
