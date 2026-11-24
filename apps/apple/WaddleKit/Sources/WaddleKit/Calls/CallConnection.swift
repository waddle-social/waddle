/// How good our own connection to a call is, as the media server judges it.
public enum CallConnectionQuality: Equatable, Sendable {
    case excellent, good, poor, lost
    /// Not measured yet, as at the start of a call.
    case unknown
}

/// The connection indicator in a call's header (web `qualityToChip`):
/// three bars that say nothing while the connection is healthy, and a
/// label once it is not. A call that is reconnecting has no chip: its
/// header says so itself.
public struct CallConnectionChip: Equatable, Sendable {
    public enum Tone: Equatable, Sendable {
        case neutral, warning, danger
    }

    /// How many of the three bars are filled.
    public let bars: Int
    public let tone: Tone
    /// Shown, and spoken, only when the connection needs saying something about.
    public let label: String?

    public init(quality: CallConnectionQuality) {
        switch quality {
        case .excellent: (bars, tone, label) = (3, .neutral, nil)
        case .good: (bars, tone, label) = (2, .neutral, nil)
        case .poor: (bars, tone, label) = (1, .warning, "Poor connection")
        case .lost: (bars, tone, label) = (0, .danger, "Connection lost")
        case .unknown: (bars, tone, label) = (0, .neutral, nil)
        }
    }

    /// Excellent or good: the bars are drawn in the colour of being available.
    public var isHealthy: Bool { bars > 0 && label == nil }

    /// The whole status, for a tooltip and for someone who asks VoiceOver.
    /// No bars and no label is the start of a call: it is measuring, not good.
    public var status: String {
        if let label { return label }
        switch bars {
        case 3: return "Excellent connection"
        case 2: return "Good connection"
        default: return "Measuring the connection…"
        }
    }
}

/// Whether the rating the SDK holds may be shown. After a reconnect it
/// still holds the old connection's rating, and it speaks up only when the
/// rating changes: so the old one is held back until the server rates the
/// new connection differently, or long enough has passed that an unchanged
/// rating is the new connection's too.
public struct CallConnectionRatingGate: Equatable, Sendable {
    /// The server rates a connection every few seconds; by now it has rated the new one.
    public static let settleTime = Duration.seconds(6)

    public private(set) var isHoldingBack = false

    public init() { /* A connection that has not dropped. */ }

    public mutating func reconnecting() { isHoldingBack = true }
    /// The server sent a rating that differs from the one held.
    public mutating func rated() { isHoldingBack = false }
    /// `settleTime` has passed since the call came back.
    public mutating func settled() { isHoldingBack = false }

    public func shown(_ held: CallConnectionQuality) -> CallConnectionQuality {
        isHoldingBack ? .unknown : held
    }
}
