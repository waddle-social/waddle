import WaddleKit

/// XEP-0353 invitations use bare or full JIDs according to each verb;
/// XEP-0166 session verbs preserve both bound resource identities.
extension FFIXmppPort {
    func reportCallLifecycle(_ report: CallLifecycleReport) async -> CallLifecycleReportResult {
        let kind: WaddleCallLifecycleKind = switch report.kind {
        case .direct: .dm
        case .group: .muc
        }
        let setup: WaddleCallSetupOutcome = switch report.setupOutcome {
        case .proposed: .proposed
        case .accepted: .accepted
        case .declined: .declined
        case .timeout: .timeout
        case .failed: .failed
        }
        let end: WaddleCallEndReason = switch report.endReason {
        case .hangup: .hangup
        case .peerLeft: .peerLeft
        case .error: .error
        case .reconnectExhausted: .reconnectExhausted
        }
        let duration: WaddleCallDurationBucket = switch report.duration {
        case .none: .none
        case .underOneMinute: .underOneMinute
        case .oneToTenMinutes: .oneToTenMinutes
        case .tenToSixtyMinutes: .tenToSixtyMinutes
        case .overSixtyMinutes: .overSixtyMinutes
        }
        let reconnects: WaddleCallReconnectBucket = switch report.reconnects {
        case .none: .none
        case .once: .once
        case .multiple: .multiple
        }
        let outcome = await client.reportCallLifecycle(report: WaddleCallLifecycleReport(
            kind: kind, setupOutcome: setup, endReason: end, duration: duration, reconnects: reconnects))
        switch outcome {
        case .accepted: return .accepted
        case .unsupported: return .unsupported
        case .rateLimited: return .rateLimited
        case .failed: return .failed
        }
    }

    func fetchExternalServices() async -> [ExternalService] {
        do {
            return try await client.fetchExternalServices().map(FFIInbound.externalService)
        } catch {
            BridgeLog.debug("external services lookup failed: \(error)")
            return []
        }
    }

    func sendCallPropose(to peer: BareJID, sid: String, media: CallMedia) async -> Bool {
        await client.sendCallPropose(peerBareJid: peer.description, sid: sid, audio: media.audio, video: media.video)
    }

    func sendCallRinging(to peer: JID, sid: String) async -> Bool {
        await client.sendCallRinging(peerFullJid: peer.description, sid: sid)
    }

    func peerReceivesOurPresence(_ peer: BareJID) async -> Bool {
        do {
            return try await client.fetchRosterContacts().contains {
                $0.receivesOurPresence && FFIInbound.bareJID($0.jid) == peer
            }
        } catch {
            BridgeLog.debug("roster lookup for the call gate failed: \(error)")
            return false
        }
    }

    func sendCallProceed(to peer: JID, sid: String) async -> Bool {
        await client.sendCallProceed(peerFullJid: peer.description, sid: sid)
    }

    func sendCallReject(to peer: JID, sid: String, tieBreak: Bool) async -> Bool {
        if tieBreak {
            return await client.sendCallRejectTieBreak(peerFullJid: peer.description, sid: sid)
        }
        return await client.sendCallReject(peerFullJid: peer.description, sid: sid)
    }

    func sendCallRetract(to peer: BareJID, sid: String) async -> Bool {
        await client.sendCallRetract(peerBareJid: peer.description, sid: sid)
    }

    func sendCallRetractTieBreak(to peer: JID, sid: String) async -> Bool {
        await client.sendCallRetractTieBreak(peerFullJid: peer.description, sid: sid)
    }

    func sendCallFinish(to peer: JID, sid: String, reason: JingleReason?) async -> Bool {
        if let reason {
            return await client.sendCallFinishWithReason(peerFullJid: peer.description, sid: sid, reason: FFIOutbound.jingleReason(reason))
        }
        return await client.sendCallFinish(peerFullJid: peer.description, sid: sid)
    }

    func sendCallFinishMigrated(to peer: JID, oldSid: String, newSid: String) async -> Bool {
        await client.sendCallFinishMigrated(peerFullJid: peer.description, oldSid: oldSid, newSid: newSid)
    }

    func sendCallSessionInitiate(to peer: JID, initiator: JID, sid: String, media: CallMedia) async -> Bool {
        await client.sendCallSessionInitiate(
            peerFullJid: peer.description, initiatorFullJid: initiator.description,
            sid: sid, audio: media.audio, video: media.video
        )
    }

    func sendCallSessionAccept(to peer: JID, responder: JID, sid: String, media: CallMedia) async -> Bool {
        await client.sendCallSessionAccept(
            peerFullJid: peer.description, responderFullJid: responder.description,
            sid: sid, audio: media.audio, video: media.video
        )
    }

    func sendCallSessionTerminate(to peer: JID, sid: String, reason: JingleReason?) async -> CallTerminateOutcome {
        let outcome = await client.sendCallSessionTerminateWithOutcome(
            peerFullJid: peer.description, sid: sid, reason: reason.map(FFIOutbound.jingleReason)
        )
        return FFIInbound.callTerminateOutcome(outcome)
    }

    /// The Rust core owns the XEP-0272 wire shape; leaving clears both markers.
    func updateGroupCallPresence(_ presence: GroupCallPresenceUpdate, in room: BareJID, nick: String) async -> Bool {
        let active: Bool
        let preparing: Bool
        let video: Bool
        switch presence.state {
        case .preparing:
            (active, preparing, video) = (false, true, false)
        case .active(let hasVideo):
            (active, preparing, video) = (true, false, hasVideo)
        case .left:
            (active, preparing, video) = (false, false, false)
        }
        let isLeaving = presence.state == .left
        do {
            try await client.updateMujiPresence(
                roomJid: room.description, nick: nick, active: active, preparing: preparing, video: video,
                flags: WaddleInCallPresenceFlags(
                    handRaised: !isLeaving && presence.isHandRaised,
                    muted: !isLeaving && presence.isMuted))
            return true
        } catch {
            BridgeLog.debug("muji presence update failed: \(error)")
            return false
        }
    }

    func sendGroupCallSessionInitiate(in room: BareJID, initiator: JID, sid: String, video: Bool) async -> Bool {
        do {
            try await client.sendMujiSessionInitiate(
                roomJid: room.description, initiatorFullJid: initiator.description, sid: sid, video: video)
            return true
        } catch {
            BridgeLog.debug("muji session initiate failed: \(error)")
            return false
        }
    }

    func sendGroupCallSessionTerminate(in room: BareJID, sid: String) async -> Bool {
        do {
            try await client.sendMujiSessionTerminate(roomJid: room.description, sid: sid)
            return true
        } catch WaddleError.Stanza(condition: "item-not-found", text: _) {
            // XEP-0166 unknown-session: the mixer has already dropped the
            // session. The leave presence that precedes a terminate does
            // exactly that, so this is the usual answer, not a failure.
            return true
        } catch {
            BridgeLog.debug("muji session terminate failed: \(error)")
            return false
        }
    }
}
