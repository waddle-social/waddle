import Foundation

/// XEP-0055 directory results for `@` mentions in rooms, so people with no
/// MUC presence (offline members) can be mentioned. Results are cached per
/// room and query, and every user a room's searches found stays
/// addressable by username when that room's messages are sent.
public struct MentionDirectorySearch: Sendable {
    private var results: [BareJID: [String: [UserSearchResult]]] = [:]
    private var seen: [BareJID: [BareJID: UserSearchResult]] = [:]

    public init() {}

    /// The cache key for a typed query: trimmed and case-folded.
    public static func key(_ query: String) -> String {
        query.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
    }

    /// The cached results for `query` in `room`, or nil when it has not
    /// been searched there.
    public func cached(_ query: String, in room: BareJID) -> [UserSearchResult]? {
        results[room]?[Self.key(query)]
    }

    public mutating func record(_ found: [UserSearchResult], for query: String, in room: BareJID) {
        results[room, default: [:]][Self.key(query)] = found
        for user in found {
            seen[room, default: [:]][user.jid] = user
        }
    }

    /// Every directory user this room's searches have returned, by username.
    public func seenUsers(in room: BareJID) -> [UserSearchResult] {
        (seen[room].map { Array($0.values) } ?? [])
            .sorted { ($0.username.lowercased(), $0.jid) < ($1.username.lowercased(), $1.jid) }
    }

    public mutating func clear() {
        results.removeAll()
        seen.removeAll()
    }

    /// The directory results the `@` popover adds after the occupants it
    /// already lists: in server order, without our own account, anyone
    /// already listed, or duplicates.
    public static func additions(
        _ found: [UserSearchResult],
        listed: some Sequence<BareJID>,
        ownJID: BareJID?
    ) -> [UserSearchResult] {
        var excluded = Set(listed)
        if let ownJID { excluded.insert(ownJID) }
        return found.filter { excluded.insert($0.jid).inserted }
    }
}
