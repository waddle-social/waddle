import Foundation
import WaddleKit

/// A row in the mention autocomplete list.
struct MentionCandidate: Hashable, Identifiable {
    let id: String
    /// Inserted after the `@`.
    let name: String
    let detail: String?
    let target: MentionTarget
    /// The occupant's real JID, for the avatar.
    let jid: BareJID?

    var token: String { "@" + name }
}

/// Autocomplete candidates for a room. Only occupants whose real JID is
/// known can be mentioned, because a XEP-0372 mention URI names a JID.
/// XEP-0055 directory results for the query follow the occupants, so
/// people with no presence in the room can be mentioned too.
enum MentionCandidates {
    static func matching(
        _ query: String,
        occupants: [Occupant],
        ownNick: String,
        directoryUsers: [UserSearchResult] = [],
        ownJID: BareJID? = nil,
        limit: Int = 8
    ) -> [MentionCandidate] {
        let broadcasts = broadcastCandidates.filter { matches(query, $0.name) }
        let people = occupants
            .filter { $0.nick != ownNick && $0.realJID != nil }
            .filter { matches(query, $0.nick) || matches(query, $0.realJID?.localpart ?? "") }
            .sorted { lhs, rhs in
                let left = (matches(query, lhs.nick) ? 0 : 1, lhs.nick.lowercased())
                let right = (matches(query, rhs.nick) ? 0 : 1, rhs.nick.lowercased())
                return left < right
            }
            .compactMap(candidate)
        let absent = MentionDirectorySearch
            .additions(directoryUsers, listed: people.compactMap(\.jid), ownJID: ownJID)
            .map(directoryCandidate)
        return Array((people + absent + broadcasts).prefix(limit))
    }

    static let broadcastCandidates = [
        MentionCandidate(id: "broadcast:everyone", name: "everyone", detail: "Notify everyone in this channel", target: .everyone, jid: nil),
        MentionCandidate(id: "broadcast:here", name: "here", detail: "Notify everyone who is online", target: .here, jid: nil),
    ]

    private static func candidate(_ occupant: Occupant) -> MentionCandidate? {
        guard let jid = occupant.realJID else { return nil }
        return MentionCandidate(id: "nick:\(occupant.nick)", name: occupant.nick, detail: jid.description, target: .user(jid), jid: jid)
    }

    private static func directoryCandidate(_ user: UserSearchResult) -> MentionCandidate {
        MentionCandidate(id: "jid:\(user.jid)", name: user.username, detail: user.jid.description, target: .user(user.jid), jid: user.jid)
    }

    private static func matches(_ query: String, _ value: String) -> Bool {
        guard !query.isEmpty else { return true }
        return value.range(of: query, options: [.caseInsensitive, .diacriticInsensitive, .anchored]) != nil
    }
}
