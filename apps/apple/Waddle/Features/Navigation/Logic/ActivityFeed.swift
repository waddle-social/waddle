import Foundation
import WaddleKit

/// A conversation that needs attention.
struct ActivityEntry: Identifiable, Hashable {
    let conversation: ConversationID
    let title: String
    let unread: Int
    let isMention: Bool
    /// Newest known activity; nil when nothing is loaded yet.
    let recency: Date?

    var id: ConversationID { conversation }
}

/// Mentions first, then other unread conversations, each most recent
/// first.
struct ActivityFeed: Equatable {
    var mentions: [ActivityEntry]
    var unread: [ActivityEntry]

    var isEmpty: Bool { mentions.isEmpty && unread.isEmpty }

    /// Every conversation "Mark all as read" should clear.
    var conversations: [ConversationID] {
        (mentions + unread).map(\.conversation)
    }

    static func build(
        counts: [ConversationID: Int],
        mentions: Set<ConversationID>,
        recency: (ConversationID) -> Date?,
        title: (ConversationID) -> String
    ) -> ActivityFeed {
        let unreadIDs = counts.filter { $0.value > 0 }.map(\.key)
        let all = Set(unreadIDs).union(mentions)
        let entries = all.map { conversation in
            ActivityEntry(
                conversation: conversation,
                title: title(conversation),
                unread: counts[conversation] ?? 0,
                isMention: mentions.contains(conversation),
                recency: recency(conversation)
            )
        }
        let ordered = entries.sorted(by: isOrderedBefore)
        return ActivityFeed(
            mentions: ordered.filter(\.isMention),
            unread: ordered.filter { !$0.isMention }
        )
    }

    /// Newest first; unknown recency last; then by title for stability.
    static func isOrderedBefore(_ lhs: ActivityEntry, _ rhs: ActivityEntry) -> Bool {
        switch (lhs.recency, rhs.recency) {
        case let (left?, right?) where left != right:
            return left > right
        case (.some, nil):
            return true
        case (nil, .some):
            return false
        default:
            let left = lhs.title.lowercased()
            let right = rhs.title.lowercased()
            if left != right { return left < right }
            return lhs.conversation.description < rhs.conversation.description
        }
    }
}
