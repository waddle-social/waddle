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
        let absent = MentionDirectorySearch
            .additions(directoryUsers, listed: people.compactMap(\.jid), ownJID: ownJID, reservedNames: labels)
            .map { MentionCandidate(name: $0.username, target: .user($0.jid)) }
        return Array((broadcasts + matchingPeople + absent).prefix(ComposerSuggestion.maximumRows))
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
