/// How the media session is doing, as the UI needs it.
public enum CallMediaPhase: Hashable, Sendable { case idle, connecting, connected, reconnecting }

/// How a call in session is doing, as every surface that shows one says
/// it: the stage, the sidebar's card and the docks. The one place the
/// media phase is read as a status.
public enum CallSessionStatus: Hashable, Sendable, CaseIterable {
    case connecting, connected, reconnecting

    /// Media is idle for a moment after a session is established, while
    /// the store fetches ICE servers: idle there still reads as connecting.
    public init(_ phase: CallMediaPhase) {
        switch phase {
        case .idle, .connecting: self = .connecting
        case .connected: self = .connected
        case .reconnecting: self = .reconnecting
        }
    }

    /// Media has connected, even if it is reconnecting now: the devices can be switched.
    public var hasMedia: Bool { self != .connecting }

    /// What a screen reader is told when the status changes.
    public var announcement: String {
        switch self {
        case .connecting: "Connecting"
        case .connected: "Call connected"
        case .reconnecting: "Reconnecting"
        }
    }
}

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
            self = Self.inSession(with: call.peer.bare, media: call.media, status: CallSessionStatus(media))
        // The dedicated group surface is #2495; the existing dock can end this call.
        case .joiningGroup(let call):
            self = .connecting(peer: call.room, media: call.media)
        case .inGroup(let call):
            self = Self.inSession(with: call.room, media: call.media, status: CallSessionStatus(media))
        case .ended(_, let reason):
            self = .ended(reason)
        }
    }

    private static func inSession(with peer: BareJID, media: CallMedia, status: CallSessionStatus) -> CallStage {
        switch status {
        case .connecting: .connecting(peer: peer, media: media)
        case .connected: .inCall(peer: peer, media: media)
        case .reconnecting: .reconnecting(peer: peer, media: media)
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
