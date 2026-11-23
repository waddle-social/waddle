/// What the call surface shows for a room's call. Derived, never stored.
public enum GroupCallStage: Hashable, Sendable {
    /// XEP-0272 setup: our presence is going out and the mixer has not accepted yet.
    case joining(room: BareJID, media: CallMedia)
    /// The mixer accepted; media is not connected yet.
    case connecting(room: BareJID, media: CallMedia)
    case inCall(room: BareJID, media: CallMedia)
    case reconnecting(room: BareJID, media: CallMedia)
}

public extension GroupCallStage {
    /// Only a group phase of the slot has a group stage; media refines only
    /// an accepted session, as in `CallStage`.
    ///
    /// | slot          | media            | stage           |
    /// |---------------|------------------|-----------------|
    /// | joiningGroup  | any              | `.joining`      |
    /// | inGroup       | idle, connecting | `.connecting`   |
    /// | inGroup       | connected        | `.inCall`       |
    /// | inGroup       | reconnecting     | `.reconnecting` |
    /// | anything else | any              | `nil`           |
    init?(state: CallState, media: CallMediaPhase) {
        switch state {
        case .joiningGroup(let call):
            self = .joining(room: call.room, media: call.media)
        case .inGroup(let call):
            switch media {
            case .idle, .connecting: self = .connecting(room: call.room, media: call.media)
            case .connected: self = .inCall(room: call.room, media: call.media)
            case .reconnecting: self = .reconnecting(room: call.room, media: call.media)
            }
        case .idle, .incoming, .outgoing, .active, .ended:
            return nil
        }
    }

    var room: BareJID {
        switch self {
        case .joining(let room, _), .connecting(let room, _), .inCall(let room, _), .reconnecting(let room, _): room
        }
    }

    var media: CallMedia {
        switch self {
        case .joining(_, let media), .connecting(_, let media), .inCall(_, let media), .reconnecting(_, let media): media
        }
    }

    /// The mixer has accepted the call, so it can have a stage while media connects.
    var isInSession: Bool {
        switch self {
        case .connecting, .inCall, .reconnecting: true
        case .joining: false
        }
    }

    /// The room's media has connected, even if it is reconnecting now: the
    /// SFU's participants are known and the devices can be switched.
    var hasMedia: Bool {
        switch self {
        case .inCall, .reconnecting: true
        case .joining, .connecting: false
        }
    }
}
