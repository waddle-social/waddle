/// How the media session is doing, as the UI needs it.
public enum CallMediaPhase: Hashable, Sendable { case idle, connecting, connected, reconnecting }

/// What the call surface shows. Derived, never stored.
public enum CallStage: Hashable, Sendable {
    /// Ringing here.
    case incoming(from: JID, media: CallMedia, isAccepting: Bool)
    /// Our proposal is out; no device of the peer's rings yet.
    case calling(to: BareJID, media: CallMedia)
    /// A peer device rings, or one answered and the session is being set up.
    case ringing(to: BareJID, media: CallMedia)
    /// The session is established; media is not connected yet.
    case connecting(peer: BareJID, media: CallMedia)
    case inCall(peer: BareJID, media: CallMedia)
    case reconnecting(peer: BareJID, media: CallMedia)
    case ended(CallEndReason)
}

public extension CallStage {
    /// The slot decides the stage; media refines only an active session.
    ///
    /// | slot                                      | media                 | stage           |
    /// |-------------------------------------------|-----------------------|-----------------|
    /// | idle                                      | any                   | `nil`           |
    /// | incoming                                  | any                   | `.incoming`     |
    /// | outgoing, no `<ringing/>`, no responder   | any                   | `.calling`      |
    /// | outgoing, `<ringing/>` or a responder     | any                   | `.ringing`      |
    /// | active                                    | idle, connecting      | `.connecting`   |
    /// | active                                    | connected             | `.inCall`       |
    /// | active                                    | reconnecting          | `.reconnecting` |
    /// | ended                                     | any                   | `.ended`        |
    ///
    /// Media is idle for a moment after the session goes active, while the
    /// store fetches ICE servers, so idle there still reads as connecting.
    init?(state: CallState, media: CallMediaPhase) {
        switch state {
        case .idle:
            return nil
        case .incoming(let call):
            self = .incoming(from: call.from, media: call.media, isAccepting: call.isAccepting)
        case .outgoing(let call) where call.isRinging || call.responder != nil:
            self = .ringing(to: call.to, media: call.media)
        case .outgoing(let call):
            self = .calling(to: call.to, media: call.media)
        case .active(let call):
            switch media {
            case .idle, .connecting: self = .connecting(peer: call.peer.bare, media: call.media)
            case .connected: self = .inCall(peer: call.peer.bare, media: call.media)
            case .reconnecting: self = .reconnecting(peer: call.peer.bare, media: call.media)
            }
        case .ended(_, let reason):
            self = .ended(reason)
        }
    }
}

public extension CallStage {
    /// The account on the other end; an ended stage no longer names one.
    var peer: BareJID? {
        switch self {
        case .incoming(let from, _, _): from.bare
        case .calling(let peer, _), .ringing(let peer, _): peer
        case .connecting(let peer, _), .inCall(let peer, _), .reconnecting(let peer, _): peer
        case .ended: nil
        }
    }

    /// The Jingle session is established, whatever its media is doing.
    var isInSession: Bool {
        switch self {
        case .connecting, .inCall, .reconnecting: true
        case .incoming, .calling, .ringing, .ended: false
        }
    }
}
