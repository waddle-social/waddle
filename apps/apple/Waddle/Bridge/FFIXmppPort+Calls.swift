import WaddleKit

/// XEP-0353 invitations use bare or full JIDs according to each verb;
/// XEP-0166 session verbs preserve both bound resource identities.
extension FFIXmppPort {
    func fetchExternalServices() async -> [ExternalService] {
        do {
            return try await client.fetchExternalServices().map(FFIInbound.externalService)
        } catch {
            BridgeLog.debug("external services lookup failed")
            return []
        }
    }

    func sendCallPropose(to peer: BareJID, sid: String, media: CallMedia) async -> Bool {
        await client.sendCallPropose(peerBareJid: peer.description, sid: sid, audio: media.audio, video: media.video)
    }

    func sendCallRinging(to peer: BareJID, sid: String) async -> Bool {
        await client.sendCallRinging(peerBareJid: peer.description, sid: sid)
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
}
