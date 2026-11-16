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
        setGroupMarkers(hand: false, muted: !media.audio)
        let identity = ownJID()
        guard groupStillPending(attempt, epoch: epoch) else { return false }
        return await runGroupSetup(attempt, identity: identity, epoch: epoch)
    }

    /// Room presence is parsed at the bridge, including absence as a leave marker.
    public func handleGroupPresence(_ presence: WirePresence) {
        groupPresence.apply(presence)
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
        _ = claimHangUp()
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
        let cleared = await groupControl.serializePresence { [self] in
            guard generation == epoch, !groupSlotHolds(room) else { return nil }
            return await port.updateGroupCallPresence(.init(state: .left), in: room, nick: nick)
        }
        guard generation == epoch, let cleared else { return }
        if !cleared { reportError("muji leave presence failed", epoch: epoch) }
        // A successor may have claimed the room while the leave was in flight.
        if !groupSlotHolds(room) { groupPresence.clearParticipant(in: room, nick: nick, realJID: identity) }
        if !state.isGroupPhase { setGroupMarkers(hand: false, muted: false) }
    }

    func terminateGroup(in room: BareJID, sid: String, epoch: Int) async {
        guard generation == epoch else { return }
        _ = await send(epoch: epoch, error: "muji session terminate failed") {
            await port.sendGroupCallSessionTerminate(in: room, sid: sid)
        }
    }

    /// The caller has already claimed the slot; leave always precedes terminate.
    func teardownGroup(_ claimed: CallState, identity: JID?, epoch: Int) async {
        switch claimed {
        case .joiningGroup(let call):
            groupPresence.cancelPreparationWaiters(in: call.room, nick: call.nick)
            cancelGroupAccept(call.sid)
            await leaveGroupPresence(in: call.room, nick: call.nick, identity: identity, epoch: epoch)
            if call.isAdvertised { await terminateGroup(in: call.room, sid: call.sid, epoch: epoch) }
        case .inGroup(let call):
            cancelGroupAccept(call.sid)
            await leaveGroupPresence(in: call.room, nick: call.nick, identity: identity, epoch: epoch)
            await terminateGroup(in: call.room, sid: call.sid, epoch: epoch)
        case .idle, .incoming, .outgoing, .active, .ended: break
        }
    }
}

extension CallStore {
    /// Only the account's mixer can end a group sid; peer and sibling ends cannot.
    func endGroupFromRemote(_ event: CallEvent, epoch: Int) async -> Bool {
        guard generation == epoch, state.isGroupPhase, state.sid == event.sid,
            event.from.bare == groupControl.mixer else { return false }
        let reason: CallEndReason
        switch event.kind {
        case .sessionTerminate(let condition), .finish(let condition, _): reason = .finished(condition)
        case .reject(let condition, let tieBreak): reason = tieBreak && condition == .expired ? .expired : .rejected
        case .retract(let condition, let tieBreak): reason = tieBreak && condition == .expired ? .expired : .retracted
        case .propose, .ringing, .proceed, .sessionInitiate, .sessionAccept: return false
        }
        let claimed = state
        transition(.ended(sid: event.sid, reason: reason))
        await teardownGroup(claimed, identity: ownJID(), epoch: epoch)
        return true
    }
}
