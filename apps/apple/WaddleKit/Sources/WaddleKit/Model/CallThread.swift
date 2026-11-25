import Foundation

/// `urn:waddle:call-thread:0`: the mark on the message a room posts when a
/// call starts in it. The message's XEP-0201 thread is the call's chat.
public struct CallThreadAnchor: Hashable, Sendable {
    public enum Kind: Hashable, Sendable {
        case room, direct
    }

    public var kind: Kind
    public var hasVideo: Bool
    /// Who started it, as the server says.
    public var initiator: BareJID?
    public var started: Date?

    public init(kind: Kind, hasVideo: Bool, initiator: BareJID? = nil, started: Date? = nil) {
        self.kind = kind
        self.hasVideo = hasVideo
        self.initiator = initiator
        self.started = started
    }
}

/// The XEP-0422 fastening a room posts when its call is over: which
/// anchor, and how long the call ran.
public struct CallThreadEnded: Hashable, Sendable {
    /// The anchor's XEP-0359 origin id.
    public var anchorID: String
    public var ended: Date?
    /// Seconds; `nil` when the duration could not be read.
    public var duration: TimeInterval?

    public init(anchorID: String, ended: Date? = nil, duration: TimeInterval? = nil) {
        self.anchorID = anchorID
        self.ended = ended
        self.duration = duration
    }

    /// Seconds in an ISO 8601 duration of hours, minutes and seconds, the
    /// only parts the server writes: `PT1H2M3S`, `PT5M0S`, `PT42S`.
    public static func seconds(iso duration: String) -> TimeInterval? {
        guard duration.hasPrefix("PT"), duration.count > 2 else { return nil }
        var total: TimeInterval = 0
        var digits = ""
        var lastUnit = 0
        for character in duration.dropFirst(2) {
            if character.isASCII, character.isNumber {
                digits.append(character)
                continue
            }
            let (unit, order): (TimeInterval, Int)
            switch character {
            case "H": (unit, order) = (3600, 1)
            case "M": (unit, order) = (60, 2)
            case "S": (unit, order) = (1, 3)
            default: return nil
            }
            // Each part once, in order, with a number before it.
            guard order > lastUnit, let value = TimeInterval(digits) else { return nil }
            total += value * unit
            lastUnit = order
            digits = ""
        }
        return digits.isEmpty && lastUnit > 0 ? total : nil
    }
}
