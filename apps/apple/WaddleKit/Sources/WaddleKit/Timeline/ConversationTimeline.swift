import Foundation
import Observation

/// The observable, ordered rows of one conversation. Written only by
/// `TimelineStore`; views observe `items`.
@MainActor
@Observable
public final class ConversationTimeline {
    public let conversation: ConversationID
    public private(set) var items: [TimelineItem] = []

    @ObservationIgnored private var indexByAnyID: [String: Int] = [:]
    @ObservationIgnored private var repliesByThread: [String: [Int]] = [:]

    init(conversation: ConversationID) {
        self.conversation = conversation
    }

    /// Rows rendered in the main feed (thread replies excluded).
    public var feedItems: [TimelineItem] {
        items.filter(\.isFeedVisible)
    }

    /// Resolves a row by any of its wire ids. Ambiguous aliases (two rows
    /// claiming the same non-primary id) resolve to nothing.
    public func item(withID id: String) -> TimelineItem? {
        indexByAnyID[id].map { items[$0] }
    }

    /// Resolves a local row key independently of its potentially colliding
    /// sender-chosen wire ids.
    public func item(withPresentationID id: String) -> TimelineItem? {
        items.first { $0.presentationID == id }
    }

    /// Replies inside the thread rooted at `threadID`, oldest first.
    public func threadReplies(threadID: String) -> [TimelineItem] {
        (repliesByThread[threadID] ?? []).map { items[$0] }
    }

    /// Number of thread replies under `item`.
    public func replyCount(for item: TimelineItem) -> Int {
        var ids = item.targetableIDs
        ids.insert(item.id)
        return ids.reduce(0) { $0 + (repliesByThread[$1]?.count ?? 0) }
    }

    /// Newest content row, used for previews and recency.
    public var lastContentItem: TimelineItem? {
        items.last(where: { $0.isFeedVisible && $0.tombstone == nil })
    }

    func publish(_ items: [TimelineItem]) {
        self.items = items
        // A room's canonical id wins over sender-chosen primary ids. A
        // colliding authored primary is ambiguous and resolves to nothing.
        var byID: [String: Int] = [:]
        var primaryOwners: [String: [Int]] = [:]
        var roomOwners: [String: [Int]] = [:]
        var aliasOwners: [String: [Int]] = [:]
        var replies: [String: [Int]] = [:]
        for (index, item) in items.enumerated() {
            if conversation.isRoom {
                primaryOwners[item.id, default: []].append(index)
                if let roomID = item.roomStanzaID {
                    roomOwners[roomID, default: []].append(index)
                }
            } else {
                byID[item.id] = index
            }
            for alias in item.targetableIDs where alias != item.id {
                aliasOwners[alias, default: []].append(index)
            }
            if let thread = item.message.thread, !item.isFeedVisible {
                replies[thread, default: []].append(index)
            }
        }
        for (id, owners) in primaryOwners where owners.count == 1 {
            byID[id] = owners[0]
        }
        for (alias, owners) in aliasOwners where byID[alias] == nil && owners.count == 1 {
            byID[alias] = owners[0]
        }
        for (id, owners) in roomOwners {
            byID[id] = owners.count == 1 ? owners[0] : nil
        }
        indexByAnyID = byID
        repliesByThread = replies
    }
}
