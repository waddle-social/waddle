import Foundation

/// RFC 6121 presence availability, folded to what the UI renders.
public enum Availability: Hashable, Sendable, Comparable {
    case available
    case chat
    case away
    case extendedAway
    case doNotDisturb
    case offline

    /// Maps the `<show/>` value of an available presence (RFC 6121 §4.7.2.1).
    public init(show: String?) {
        switch show {
        case "chat": self = .chat
        case "away": self = .away
        case "xa": self = .extendedAway
        case "dnd": self = .doNotDisturb
        default: self = .available
        }
    }

    public var isOnline: Bool { self != .offline }

    /// The `<show/>` value to send for this availability; `nil` means plain
    /// available.
    public var showValue: String? {
        switch self {
        case .available, .offline: return nil
        case .chat: return "chat"
        case .away: return "away"
        case .extendedAway: return "xa"
        case .doNotDisturb: return "dnd"
        }
    }
}

/// XEP-0045 affiliation.
public enum RoomAffiliation: Hashable, Sendable, Comparable {
    case owner
    case admin
    case member
    case none
    case outcast
}

/// XEP-0045 role.
public enum RoomRole: Hashable, Sendable, Comparable {
    case moderator
    case participant
    case visitor
    case none
}

/// XEP-0317 hat.
public struct Hat: Hashable, Sendable {
    public let uri: String
    public let title: String

    public init(uri: String, title: String) {
        self.uri = uri
        self.title = title
    }
}

/// A presence stanza parsed once at the FFI boundary.
public struct WirePresence: Hashable, Sendable {
    public enum Kind: Hashable, Sendable {
        case available
        case unavailable
        case error(condition: String?)
        /// Subscription management and probes; the stores ignore these.
        case other(String)

        public init(wire: String, errorCondition: String?) {
            switch wire {
            case "", "available": self = .available
            case "unavailable": self = .unavailable
            case "error": self = .error(condition: errorCondition)
            default: self = .other(wire)
            }
        }
    }

    public struct Occupant: Hashable, Sendable {
        public let affiliation: RoomAffiliation?
        public let role: RoomRole?
        /// The occupant's real JID, when the room exposes it.
        public let realJID: JID?
        /// XEP-0045 status codes (110 = self-presence, 303 = nick change…).
        public let statusCodes: Set<Int>

        public init(affiliation: RoomAffiliation?, role: RoomRole?, realJID: JID?, statusCodes: Set<Int>) {
            self.affiliation = affiliation
            self.role = role
            self.realJID = realJID
            self.statusCodes = statusCodes
        }

        public var isSelf: Bool { statusCodes.contains(110) }
    }

    public let from: JID
    public let kind: Kind
    public let show: String?
    public let status: String?
    public let hats: [Hat]
    /// Present when the presence carries a XEP-0045 `<x/>` payload.
    public let occupant: Occupant?
    /// XEP-0319 idle-since.
    public let idleSince: Date?

    public init(
        from: JID,
        kind: Kind,
        show: String? = nil,
        status: String? = nil,
        hats: [Hat] = [],
        occupant: Occupant? = nil,
        idleSince: Date? = nil
    ) {
        self.from = from
        self.kind = kind
        self.show = show
        self.status = status
        self.hats = hats
        self.occupant = occupant
        self.idleSince = idleSince
    }

    public var availability: Availability {
        switch kind {
        case .available: return Availability(show: show)
        case .unavailable, .error, .other: return .offline
        }
    }
}
