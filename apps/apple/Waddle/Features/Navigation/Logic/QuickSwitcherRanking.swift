import Foundation
import WaddleKit

/// One conversation the quick switcher can jump to.
struct QuickSwitcherEntry: Identifiable, Hashable {
    enum Kind: Hashable {
        case channel(Channel.Kind)
        case groupDM
        case direct
    }

    let conversation: ConversationID
    let title: String
    /// Space name for channels, "Group chat", or the peer address.
    let subtitle: String?
    let kind: Kind
    let unread: Int
    let mentionsMe: Bool

    var id: ConversationID { conversation }
}

/// Builds and orders quick switcher entries.
enum QuickSwitcherRanking {
    static let defaultLimit = 50

    /// Channels, then group DMs, then DMs (most recent first).
    static func entries(
        channels: [Channel],
        groupDMs: [Channel],
        directs: [DirectConversation],
        spaces: [Space],
        unread: [ConversationID: Int],
        mentions: Set<ConversationID>
    ) -> [QuickSwitcherEntry] {
        let spaceNames = Dictionary(spaces.map { ($0.id, $0.name) }, uniquingKeysWith: { first, _ in first })
        let showsSpace = spaces.count > 1
        let channelEntries = NavigationSections.sortedByPosition(channels).map { channel in
            QuickSwitcherEntry(
                conversation: channel.conversation,
                title: channel.name,
                subtitle: showsSpace ? channel.spaceID.flatMap { spaceNames[$0] } : nil,
                kind: .channel(channel.kind),
                unread: unread[channel.conversation] ?? 0,
                mentionsMe: mentions.contains(channel.conversation)
            )
        }
        let groupEntries = groupDMs.map { group in
            QuickSwitcherEntry(
                conversation: group.conversation,
                title: group.name,
                subtitle: "Group chat",
                kind: .groupDM,
                unread: unread[group.conversation] ?? 0,
                mentionsMe: mentions.contains(group.conversation)
            )
        }
        let directEntries = directs.map { direct in
            QuickSwitcherEntry(
                conversation: direct.conversation,
                title: direct.displayName,
                subtitle: direct.peer.description,
                kind: .direct,
                unread: unread[direct.conversation] ?? 0,
                mentionsMe: mentions.contains(direct.conversation)
            )
        }
        return channelEntries + groupEntries + directEntries
    }

    /// With no query: mentions, then unread, then the rest in list order.
    /// With a query: title prefix, word prefix, title substring, then
    /// subtitle substring; ties keep the no-query order.
    static func ranked(_ entries: [QuickSwitcherEntry], query: String, limit: Int = defaultLimit) -> [QuickSwitcherEntry] {
        let needle = ConversationNameFilter.normalized(query)
        let scored: [(entry: QuickSwitcherEntry, score: Int, index: Int)] = entries.enumerated().compactMap { index, entry in
            guard let score = matchScore(entry, needle: needle) else { return nil }
            return (entry, score, index)
        }
        let sorted = scored.sorted { lhs, rhs in
            let left = (lhs.score, attentionRank(lhs.entry), lhs.index)
            let right = (rhs.score, attentionRank(rhs.entry), rhs.index)
            return left < right
        }
        return sorted.prefix(max(0, limit)).map(\.entry)
    }

    /// Lower is better; nil excludes the entry.
    static func matchScore(_ entry: QuickSwitcherEntry, needle: String) -> Int? {
        if needle.isEmpty { return 0 }
        if ConversationNameFilter.hasPrefix(entry.title, needle) { return 0 }
        if wordPrefix(entry.title, needle) { return 1 }
        if ConversationNameFilter.contains(entry.title, needle) { return 2 }
        if let subtitle = entry.subtitle, ConversationNameFilter.contains(subtitle, needle) { return 3 }
        return nil
    }

    private static func attentionRank(_ entry: QuickSwitcherEntry) -> Int {
        if entry.mentionsMe { return 0 }
        if entry.unread > 0 { return 1 }
        return 2
    }

    private static func wordPrefix(_ title: String, _ needle: String) -> Bool {
        let words = title.split(whereSeparator: { !$0.isLetter && !$0.isNumber })
        return words.contains { ConversationNameFilter.hasPrefix(String($0), needle) }
    }
}

/// Keyboard highlight movement in the quick switcher.
enum QuickSwitcherCursor {
    /// Keeps the highlight when it is still listed, else the first row.
    static func resolved(_ current: ConversationID?, in ids: [ConversationID]) -> ConversationID? {
        if let current, ids.contains(current) { return current }
        return ids.first
    }

    /// Moves by `offset` rows, wrapping at either end.
    static func moved(_ current: ConversationID?, by offset: Int, in ids: [ConversationID]) -> ConversationID? {
        guard !ids.isEmpty else { return nil }
        guard let current, let index = ids.firstIndex(of: current) else {
            return offset < 0 ? ids.last : ids.first
        }
        let count = ids.count
        let next = ((index + offset) % count + count) % count
        return ids[next]
    }
}
