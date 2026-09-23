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
