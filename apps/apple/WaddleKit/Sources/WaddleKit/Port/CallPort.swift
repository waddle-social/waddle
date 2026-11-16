/// XEP-0353 call invitations and the XEP-0166 session verbs. `false` means the stanza was not written.
public protocol CallPort: AnyObject, Sendable {
    /// XEP-0215 STUN/TURN advertisement; failure returns an empty list so
    /// media keeps the SDK's signaling-provided servers.
    func fetchExternalServices() async -> [ExternalService]

    func sendCallPropose(to peer: BareJID, sid: String, media: CallMedia) async -> Bool
    func sendCallRinging(to peer: BareJID, sid: String) async -> Bool
    func sendCallProceed(to peer: JID, sid: String) async -> Bool
    func sendCallReject(to peer: JID, sid: String, tieBreak: Bool) async -> Bool
    func sendCallRetract(to peer: BareJID, sid: String) async -> Bool
    func sendCallRetractTieBreak(to peer: JID, sid: String) async -> Bool
    func sendCallFinish(to peer: JID, sid: String, reason: JingleReason?) async -> Bool
    func sendCallFinishMigrated(to peer: JID, oldSid: String, newSid: String) async -> Bool
    func sendCallSessionInitiate(to peer: JID, initiator: JID, sid: String, media: CallMedia) async -> Bool
    func sendCallSessionAccept(to peer: JID, responder: JID, sid: String, media: CallMedia) async -> Bool
    func sendCallSessionTerminate(to peer: JID, sid: String, reason: JingleReason?) async -> CallTerminateOutcome
}
