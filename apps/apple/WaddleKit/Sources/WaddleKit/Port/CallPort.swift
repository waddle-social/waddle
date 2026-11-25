/// XEP-0353 call invitations and the XEP-0166 session verbs. `false` means the stanza was not written.
public protocol CallPort: AnyObject, Sendable {
    /// Each outcome is attempted once; delivery never retries a report.
    func reportCallLifecycle(_ report: CallLifecycleReport) async -> CallLifecycleReportResult

    /// XEP-0215 STUN/TURN advertisement; failure returns an empty list so
    /// media keeps the SDK's signaling-provided servers.
    func fetchExternalServices() async -> [ExternalService]

    /// XEP-0272 room presence; false means the stanza was not written.
    func updateGroupCallPresence(_ presence: GroupCallPresenceUpdate, in room: BareJID, nick: String) async -> Bool
    /// Session IQs return true only when the mixer acknowledged them.
    func sendGroupCallSessionInitiate(in room: BareJID, initiator: JID, sid: String, video: Bool) async -> Bool
    func sendGroupCallSessionTerminate(in room: BareJID, sid: String) async -> Bool

    /// `urn:waddle:in-call:0`: to the device we are in a call of two with,
    /// or to a room for everyone in its call.
    func sendInCallReaction(_ emoji: String, to destination: CallReactionLedger.Destination) async -> Bool

    func sendCallPropose(to peer: BareJID, sid: String, media: CallMedia) async -> Bool
    /// XEP-0353 §3.2: addressed to the proposer's full JID.
    func sendCallRinging(to peer: JID, sid: String) async -> Bool
    /// Whether `peer` is a roster contact that receives this account's presence
    /// (subscription `from` or `both`). Any failure answers false: the
    /// automatic `<ringing/>` to a stranger is a presence leak (XEP-0353 Security).
    func peerReceivesOurPresence(_ peer: BareJID) async -> Bool
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

/// The Rust core builds Muji contents; leaving drops every in-call marker.
public struct GroupCallPresenceUpdate: Hashable, Sendable {
    public enum State: Hashable, Sendable { case preparing, active(video: Bool), left }
    public let state: State
    public let isHandRaised: Bool
    public let isMuted: Bool

    public init(state: State, isHandRaised: Bool = false, isMuted: Bool = false) {
        self.state = state
        self.isHandRaised = state == .left ? false : isHandRaised
        self.isMuted = state == .left ? false : isMuted
    }
}
