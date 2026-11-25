import WaddleKit

/// Wire-level observations shared by the store and session coordinator suites.
enum CallSend: Hashable {
    case groupPresence(BareJID, String, GroupCallPresenceUpdate)
    case groupInitiate(BareJID, JID, String, Bool)
    case groupTerminate(BareJID, String)
    case propose(BareJID, String, CallMedia)
    case ringing(JID, String)
    case reaction(String, CallReactionLedger.Destination)
    case proceed(JID, String)
    case reject(JID, String, Bool)
    case retract(BareJID, String)
    case retractTieBreak(JID, String)
    case finish(JID, String, JingleReason?)
    case migrated(JID, String, String)
    case initiate(JID, JID, String, CallMedia)
    case accept(JID, JID, String, CallMedia)
    case terminate(JID, String, JingleReason?)

    var verb: String {
        switch self {
        case .groupPresence: "groupPresence"
        case .groupInitiate: "groupInitiate"
        case .groupTerminate: "groupTerminate"
        case .propose: "propose"
        case .ringing: "ringing"
        case .reaction: "reaction"
        case .proceed: "proceed"
        case .reject: "reject"
        case .retract: "retract"
        case .retractTieBreak: "retractTieBreak"
        case .finish: "finish"
        case .migrated: "migrated"
        case .initiate: "initiate"
        case .accept: "accept"
        case .terminate: "terminate"
        }
    }
}

extension FakePort {
    func reportCallLifecycle(_ report: CallLifecycleReport) async -> CallLifecycleReportResult {
        callLifecycleReports.append(report)
        let result = callLifecycleReportResult
        if holdCallLifecycleReports {
            await withCheckedContinuation { heldCallLifecycleReports.append($0) }
        }
        return result
    }

    func releaseCallLifecycleReports() {
        let held = heldCallLifecycleReports
        heldCallLifecycleReports = []
        for continuation in held { continuation.resume() }
    }

    func updateGroupCallPresence(_ presence: GroupCallPresenceUpdate, in room: BareJID, nick: String) async -> Bool {
        await recordCall(.groupPresence(room, nick, presence))
    }
    func sendGroupCallSessionInitiate(in room: BareJID, initiator: JID, sid: String, video: Bool) async -> Bool {
        await recordCall(.groupInitiate(room, initiator, sid, video))
    }
    func sendGroupCallSessionTerminate(in room: BareJID, sid: String) async -> Bool {
        await recordCall(.groupTerminate(room, sid))
    }

    func fetchExternalServices() async -> [ExternalService] {
        externalServicesFetchCount += 1
        let result = externalServices
        if holdExternalServicesFetch {
            await withCheckedContinuation { heldExternalServicesFetches.append($0) }
        }
        return result
    }

    func releaseExternalServicesFetches() {
        let held = heldExternalServicesFetches
        heldExternalServicesFetches = []
        for continuation in held { continuation.resume() }
    }

    private func recordCall(_ send: CallSend) async -> Bool {
        callSends.append(send)
        let result = callResults[send.verb] ?? true
        if heldCallVerbs.contains(send.verb) {
            await withCheckedContinuation { heldCallSends.append($0) }
        }
        completedCallSends.append(send)
        return result
    }

    func releaseCallSends() {
        let held = heldCallSends
        heldCallSends = []
        for continuation in held { continuation.resume() }
    }

    func sendInCallReaction(_ emoji: String, to destination: CallReactionLedger.Destination) async -> Bool {
        await recordCall(.reaction(emoji, destination))
    }
    func sendCallPropose(to peer: BareJID, sid: String, media: CallMedia) async -> Bool {
        await recordCall(.propose(peer, sid, media))
    }
    func sendCallRinging(to peer: JID, sid: String) async -> Bool {
        await recordCall(.ringing(peer, sid))
    }
    func peerReceivesOurPresence(_ peer: BareJID) async -> Bool {
        presenceSubscribers.contains(peer)
    }
    func sendCallProceed(to peer: JID, sid: String) async -> Bool {
        await recordCall(.proceed(peer, sid))
    }
    func sendCallReject(to peer: JID, sid: String, tieBreak: Bool) async -> Bool {
        await recordCall(.reject(peer, sid, tieBreak))
    }
    func sendCallRetract(to peer: BareJID, sid: String) async -> Bool {
        await recordCall(.retract(peer, sid))
    }
    func sendCallRetractTieBreak(to peer: JID, sid: String) async -> Bool {
        await recordCall(.retractTieBreak(peer, sid))
    }
    func sendCallFinish(to peer: JID, sid: String, reason: JingleReason?) async -> Bool {
        await recordCall(.finish(peer, sid, reason))
    }
    func sendCallFinishMigrated(to peer: JID, oldSid: String, newSid: String) async -> Bool {
        await recordCall(.migrated(peer, oldSid, newSid))
    }
    func sendCallSessionInitiate(to peer: JID, initiator: JID, sid: String, media: CallMedia) async -> Bool {
        await recordCall(.initiate(peer, initiator, sid, media))
    }
    func sendCallSessionAccept(to peer: JID, responder: JID, sid: String, media: CallMedia) async -> Bool {
        await recordCall(.accept(peer, responder, sid, media))
    }
    func sendCallSessionTerminate(to peer: JID, sid: String, reason: JingleReason?) async -> CallTerminateOutcome {
        let result = callTerminateOutcome
        let sent = await recordCall(.terminate(peer, sid, reason))
        return sent ? result : .error
    }
}
