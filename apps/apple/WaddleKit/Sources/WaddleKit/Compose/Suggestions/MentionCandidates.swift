import Foundation

/// A room mention suggestion and the target to record when it is picked.
public struct MentionCandidate: Hashable, Sendable {
    public let name: String
    public let target: MentionTarget

    public init(name: String, target: MentionTarget) {
        self.name = name
        self.target = target
    }

    public var token: String { "@" + name }

    public var jid: BareJID? {
        if case let .user(jid) = target { return jid }
        return nil
    }
}

public enum MentionCandidates {
    /// Broadcasts first, then matching occupants with known addresses,
    /// then directory hits. The directory already matched its own hits.
    public static func matching(
        _ query: String,
        occupants: [Occupant],
        ownNick: String,
        directoryUsers: [UserSearchResult] = [],
        ownJID: BareJID? = nil
    ) -> [MentionCandidate] {
        let broadcasts = broadcastCandidates.filter { matches(query, $0.name) }
        var labels = Set(broadcastCandidates.map { $0.name.lowercased() })
        let people = occupants
            .filter { $0.nick != ownNick }
            .compactMap(candidate)
            .sorted {
                ($0.name.lowercased(), $0.name) < ($1.name.lowercased(), $1.name)
            }
            .filter { labels.insert($0.name.lowercased()).inserted }
        let matchingPeople = people.filter { matches(query, $0.name) || matches(query, $0.jid?.localpart ?? "") }
        let peopleByJID = Dictionary(people.compactMap { person in
            person.jid.map { ($0, person) }
        }, uniquingKeysWith: { first, _ in first })
        let directoryMatches = MentionDirectorySearch.additions(
            directoryUsers, listed: matchingPeople.compactMap(\.jid), ownJID: ownJID
        ) { user in
            if let person = peopleByJID[user.jid] { return person.name }
            return labels.contains(user.username.lowercased()) ? nil : user.username
        }.map { user in
            peopleByJID[user.jid] ?? MentionCandidate(name: user.username, target: .user(user.jid))
        }
        return Array((broadcasts + matchingPeople + directoryMatches).prefix(ComposerSuggestion.maximumRows))
    }

    private static let broadcastCandidates: [MentionCandidate] = [
        .init(name: "everyone", target: .everyone),
        .init(name: "here", target: .here),
    ]

    private static func candidate(_ occupant: Occupant) -> MentionCandidate? {
        guard let jid = occupant.realJID else { return nil }
        return MentionCandidate(name: occupant.nick, target: .user(jid))
    }

    private static func matches(_ query: String, _ value: String) -> Bool {
        query.isEmpty || value.range(of: query, options: [.caseInsensitive, .diacriticInsensitive]) != nil
    }
}
