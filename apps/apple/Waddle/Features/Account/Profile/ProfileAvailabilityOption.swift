import Foundation
import WaddleKit

/// The availabilities a user can choose for themselves (RFC 6121 `<show/>`).
enum ProfileAvailabilityOption: String, CaseIterable, Identifiable, Hashable, Sendable {
    case online
    case away
    case doNotDisturb

    var id: String { rawValue }

    var availability: Availability {
        switch self {
        case .online: return .available
        case .away: return .away
        case .doNotDisturb: return .doNotDisturb
        }
    }

    var title: String {
        switch self {
        case .online: return "Online"
        case .away: return "Away"
        case .doNotDisturb: return "Do not disturb"
        }
    }

    /// The option that represents `availability`; `chat` and `xa` fold into
    /// their nearest choice so the picker always has a selection.
    init(_ availability: Availability) {
        switch availability {
        case .available, .chat, .offline: self = .online
        case .away, .extendedAway: self = .away
        case .doNotDisturb: self = .doNotDisturb
        }
    }
}

/// What the status picker offers (ADR-010). Anything but Automatic is
/// held on every device of the account until Automatic is chosen again.
enum ProfileStatusChoice: String, CaseIterable, Identifiable, Hashable, Sendable {
    case automatic
    case online
    case away
    case doNotDisturb

    static let footer = "Automatic shows you as away when you have not used this device for 10 minutes. "
        + "A status you pick stays on all your devices until you choose Automatic again."

    var id: String { rawValue }

    var mode: PresenceMode {
        switch self {
        case .automatic: .automatic
        case .online: .manual(.available)
        case .away: .manual(.away)
        case .doNotDisturb: .manual(.doNotDisturb)
        }
    }

    var title: String {
        switch self {
        case .automatic: "Automatic"
        case .online: ProfileAvailabilityOption.online.title
        case .away: ProfileAvailabilityOption.away.title
        case .doNotDisturb: ProfileAvailabilityOption.doNotDisturb.title
        }
    }

    /// "Free to chat" can be picked on another client; here it shows as
    /// Online, which is the nearest choice.
    init(_ mode: PresenceMode) {
        switch mode {
        case .automatic: self = .automatic
        case .manual(.available), .manual(.chat): self = .online
        case .manual(.away): self = .away
        case .manual(.doNotDisturb): self = .doNotDisturb
        }
    }

    /// The dot beside the choice.
    func dot(automaticShows: Availability) -> Availability {
        switch mode {
        case .automatic: automaticShows
        case let .manual(status): status.availability
        }
    }
}

enum ProfileStatusText {
    /// The status message to send: trimmed, and nil when blank.
    static func normalized(_ text: String) -> String? {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }

    /// True when `draft` differs from what the session already has.
    static func hasChanges(draft: String, current: String?) -> Bool {
        normalized(draft) != current
    }
}
