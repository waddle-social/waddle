import Foundation
import WaddleKit

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
