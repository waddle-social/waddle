import Foundation
import WaddleKit

/// A XEP-0317 hat or XEP-0045 standing shown beside an author's name.
enum MessageRoleBadge: Hashable {
    case hat(String)
    case owner
    case admin
    case moderator

    var title: String {
        switch self {
        case let .hat(title): return title
        case .owner: return "Owner"
        case .admin: return "Admin"
        case .moderator: return "Moderator"
        }
    }

    /// A hat wins; otherwise the strongest affiliation or role.
    static func of(_ occupant: Occupant?) -> MessageRoleBadge? {
        guard let occupant else { return nil }
        if let hat = occupant.hats.first(where: { !$0.title.isEmpty }) {
            return .hat(hat.title)
        }
        switch occupant.affiliation {
        case .owner: return .owner
        case .admin: return .admin
        case .member, .none, .outcast: break
        }
        return occupant.role == .moderator ? .moderator : nil
    }
}

/// Who wrote a row, as the row renders it.
struct MessageAuthor: Hashable {
    let name: String
    /// XEP-0392 input: the bare JID when known, else the nick.
    let colorKey: String
    /// The JID whose XEP-0084 avatar to show, when known.
    let avatarJID: BareJID?
    let badge: MessageRoleBadge?

    /// `occupant` is the room occupant for the row's nick, when present;
    /// `authorJID` is `SessionCoordinator.authorJID(of:)`. Callers that
    /// only need the name may omit both.
    static func resolve(_ item: TimelineItem, occupant: Occupant?, authorJID: BareJID? = nil) -> MessageAuthor {
        let name = item.authorName.isEmpty ? "Unknown" : item.authorName
        return MessageAuthor(
            name: name,
            colorKey: authorJID?.description ?? name,
            avatarJID: authorJID,
            badge: item.conversation.isRoom ? MessageRoleBadge.of(occupant) : nil
        )
    }
}
