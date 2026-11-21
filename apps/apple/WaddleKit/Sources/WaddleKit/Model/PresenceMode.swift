import Foundation

/// A status the user picked by hand (ADR-010). Extended Away is never
/// picked: only a long idle device reaches it.
public enum ManualStatus: Hashable, Sendable {
    case available
    case chat
    case away
    case doNotDisturb

    public var availability: Availability {
        switch self {
        case .available: .available
        case .chat: .chat
        case .away: .away
        case .doNotDisturb: .doNotDisturb
        }
    }
}

/// How the account's presence is decided (ADR-010). It is the same on
/// every device of the account: `automatic` lets each device's own idle
/// time govern; `manual` holds one status everywhere until it is reset.
public enum PresenceMode: Hashable, Sendable {
    case automatic
    case manual(ManualStatus)
}

/// What this device's idle time says its presence is.
public struct AutoAway: Equatable, Sendable {
    /// Without input for this long, the device is Away.
    public static let awayAfter: TimeInterval = 10 * 60
    /// Without input for this long, the device is Extended Away.
    public static let extendedAwayAfter: TimeInterval = 30 * 60

    public static let active = AutoAway(availability: .available, idleSince: nil)

    public let availability: Availability
    /// When input stopped (XEP-0319); nil while the device is in use.
    public let idleSince: Date?

    public init(availability: Availability, idleSince: Date?) {
        self.availability = availability
        self.idleSince = idleSince
    }

    /// The stamp is `lastActive`, never `now`: contacts see when input
    /// stopped, not when this device noticed.
    public init(now: Date, lastActive: Date) {
        let idleFor = now.timeIntervalSince(lastActive)
        if idleFor >= Self.extendedAwayAfter {
            self.init(availability: .extendedAway, idleSince: lastActive)
        } else if idleFor >= Self.awayAfter {
            self.init(availability: .away, idleSince: lastActive)
        } else {
            self = .active
        }
    }
}

/// The presence this device sends for itself.
public struct OwnPresence: Equatable, Sendable {
    public let availability: Availability
    public let idleSince: Date?

    public init(availability: Availability, idleSince: Date?) {
        self.availability = availability
        self.idleSince = idleSince
    }

    /// A picked status wins and suspends auto-away, so it carries no idle
    /// stamp; automatic mode sends what the device's idle time says.
    public init(mode: PresenceMode, autoAway: AutoAway) {
        switch mode {
        case .automatic:
            self.init(availability: autoAway.availability, idleSince: autoAway.idleSince)
        case let .manual(status):
            self.init(availability: status.availability, idleSince: nil)
        }
    }
}
