import Observation

/// A 1:1 call that another of our devices is in (web `DmCallActivity` with
/// its "Other device" reading): this device cannot join it, but should not
/// pretend there is no call.
public struct CallElsewhere: Identifiable, Equatable, Sendable {
    public let sid: String
    public let peer: BareJID
    /// What the call was proposed with; `nil` when we came online after the proposal.
    public let media: CallMedia?

    public var id: String { sid }

    public init(sid: String, peer: BareJID, media: CallMedia?) {
        self.sid = sid
        self.peer = peer
        self.media = media
    }
}

/// Follows the XEP-0353 messages of calls this device is not part of, as
/// XEP-0280 carbons deliver them: an invitation one of our other devices
/// answered or placed, until someone ends it.
///
/// It keeps nothing across a lost stream: an end that was sent while we
/// were away would never be seen, and a call that is over must not be
/// shown as live.
struct CallsElsewhereLedger: Equatable, Sendable {
    /// An invitation seen and not yet answered or ended.
    struct Invitation: Equatable, Sendable {
        let peer: BareJID
        let media: CallMedia
        /// One of our other devices placed it; otherwise the peer did.
        let isOurs: Bool
    }

    /// Invitations remembered at once. An older one is forgotten; its call,
    /// if answered later, is then learned from the answer alone.
    static let invitationLimit = 32

    private(set) var invitations: [String: Invitation] = [:]
    private var invitationOrder: [String] = []
    private(set) var live: [CallElsewhere] = []

    /// - Parameters:
    ///   - own: Our account.
    ///   - device: This device's full address: what it sends itself is no other device's doing.
    mutating func apply(_ event: CallEvent, own: BareJID, device: JID?) {
        guard event.from != device else { return }
        let isOurs = event.from.bare == own
        switch event.kind {
        case .propose(let media):
            // Our other device's invitation says who it is for; without that it is nobody's.
            guard let peer = isOurs ? event.to?.bare : event.from.bare, peer != own else { return }
            remember(Invitation(peer: peer, media: media, isOurs: isOurs), sid: event.sid)
        case .proceed:
            if isOurs {
                // Another of our devices answered the peer's invitation.
                let invitation = invitations[event.sid]
                guard invitation?.isOurs != true, let peer = invitation?.peer ?? event.to?.bare, peer != own else { return }
                goLive(CallElsewhere(sid: event.sid, peer: peer, media: invitation?.media))
            } else {
                // The peer answered an invitation another of our devices placed. One
                // this device placed is its own call, and is not among the invitations.
                guard let invitation = invitations[event.sid], invitation.isOurs, invitation.peer == event.from.bare,
                      event.to == nil || event.to != device else { return }
                goLive(CallElsewhere(sid: event.sid, peer: invitation.peer, media: invitation.media))
            }
        case .reject, .retract, .finish:
            // Only a party to the call ends it: someone who learned the sid does not.
            let peer = invitations[event.sid]?.peer ?? live.first { $0.sid == event.sid }?.peer
            guard isOurs || event.from.bare == peer else { return }
            forget(event.sid)
        case .ringing, .sessionInitiate, .sessionAccept, .sessionTerminate:
            // Not news about whether there is a call. The Jingle session is
            // between the two devices in it and is not copied to us.
            return
        }
    }

    mutating func clear() {
        self = Self()
    }

    private mutating func remember(_ invitation: Invitation, sid: String) {
        guard invitations[sid] == nil, !live.contains(where: { $0.sid == sid }) else { return }
        invitations[sid] = invitation
        invitationOrder.append(sid)
        if invitationOrder.count > Self.invitationLimit {
            invitations[invitationOrder.removeFirst()] = nil
        }
    }

    private mutating func goLive(_ call: CallElsewhere) {
        dropInvitation(call.sid)
        guard !live.contains(where: { $0.sid == call.sid }) else { return }
        live.append(call)
    }

    private mutating func forget(_ sid: String) {
        dropInvitation(sid)
        live.removeAll { $0.sid == sid }
    }

    private mutating func dropInvitation(_ sid: String) {
        guard invitations.removeValue(forKey: sid) != nil else { return }
        invitationOrder.removeAll { $0 == sid }
    }
}

/// The calls our other devices are in, for whoever draws them.
@MainActor @Observable public final class CallsElsewhere {
    public private(set) var calls: [CallElsewhere] = []
    @ObservationIgnored private var ledger = CallsElsewhereLedger()

    public init() { /* no calls known yet */ }

    func apply(_ event: CallEvent, own: BareJID, device: JID?) {
        ledger.apply(event, own: own, device: device)
        publish()
    }

    func clear() {
        ledger.clear()
        publish()
    }

    /// Writes only a change, so an event that changes nothing redraws nothing.
    private func publish() {
        if calls != ledger.live { calls = ledger.live }
    }
}
