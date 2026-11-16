/// One signed-in session owns a single call slot.
public enum CallState: Hashable, Sendable {
    case idle
    case incoming(IncomingCall)
    case outgoing(OutgoingCall)
    case active(ActiveCall)
    case ended(sid: String, reason: CallEndReason)
}

/// The proposer's full JID is retained for all responses.
public struct IncomingCall: Hashable, Sendable {
    public let from: JID
    public let sid: String
    public let media: CallMedia
    public var isAccepting: Bool
    public init(from: JID, sid: String, media: CallMedia, isAccepting: Bool = false) {
        self.from = from
        self.sid = sid
        self.media = media
        self.isAccepting = isAccepting
    }
}

/// Proposals target the bare peer; the initiator names our bound resource.
public struct OutgoingCall: Hashable, Sendable {
    public let to: BareJID
    public let sid: String
    public let media: CallMedia
    public let initiator: JID?
    public var isRinging: Bool
    /// The peer device whose `<proceed/>` arrived first. Once set, the
    /// Jingle session is negotiated with it alone and the ring is answered.
    public var responder: JID?
    public init(
        to: BareJID, sid: String, media: CallMedia, initiator: JID? = nil, isRinging: Bool = false, responder: JID? = nil
    ) {
        self.to = to
        self.sid = sid
        self.media = media
        self.initiator = initiator
        self.isRinging = isRinging
        self.responder = responder
    }
}

/// Jingle established a session with server-issued media credentials.
public struct ActiveCall: Hashable, Sendable {
    public let peer: JID
    public let sid: String
    public let media: CallMedia
    public let join: CallJoin
    public let initiator: JID?
    public init(peer: JID, sid: String, media: CallMedia, join: CallJoin, initiator: JID? = nil) {
        self.peer = peer
        self.sid = sid
        self.media = media
        self.join = join
        self.initiator = initiator
    }
}

/// Why the slot ended. `expired` is a lost XEP-0353 tie-break; `finished`
/// carries the Jingle condition of a `<finish/>` or session-terminate.
public enum CallEndReason: Hashable, Sendable {
    case rejected, retracted, expired, timeout, error
    case finished(JingleReason?)
}

extension CallState {
    var sid: String? {
        switch self {
        case .idle: nil
        case .incoming(let call): call.sid
        case .outgoing(let call): call.sid
        case .active(let call): call.sid
        case .ended(let sid, _): sid
        }
    }
    /// A call is ringing, being placed or in progress: the slot is taken.
    public var isLive: Bool {
        switch self {
        case .incoming, .outgoing, .active: true
        case .idle, .ended: false
        }
    }
    var isIncoming: Bool { if case .incoming = self { true } else { false } }
    var isOutgoing: Bool { if case .outgoing = self { true } else { false } }
}
