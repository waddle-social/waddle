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
    /// XEP-0201 sub-threads by the thread their root is a reply in.
    @ObservationIgnored private var childThreads: [String: [String]] = [:]

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

    /// Every message in the thread rooted at `item` and in the sub-threads
    /// under it, each once, oldest first.
    public func threadDescendants(of item: TimelineItem) -> [TimelineItem] {
        var ids = item.targetableIDs
        ids.insert(item.id)
        var pending = Array(ids)
        var seen = Set<String>()
        var indices = Set<Int>()
        while let thread = pending.popLast() {
            guard seen.insert(thread).inserted else { continue }
            indices.formUnion(repliesByThread[thread] ?? [])
            pending.append(contentsOf: childThreads[thread] ?? [])
        }
        return indices.sorted().map { items[$0] }
    }

    /// Number of messages under `item`, its sub-threads' included.
    public func replyCount(for item: TimelineItem) -> Int {
        threadDescendants(of: item).count
    }

    /// Reply count, participants and newest reply of the thread rooted at
    /// `item` and its sub-threads. Nil when it has no loaded replies.
    public func threadSummary(for item: TimelineItem) -> ThreadSummary? {
        ThreadSummary.of(replies: threadDescendants(of: item))
    }

    /// The thread a sub-thread hangs off: the thread its root is a reply
    /// in, whether or not the sub-thread has messages yet. Nil for a
    /// top-level thread, a thread whose root is not loaded, and a thread
    /// whose parents loop back to it.
    public func parentThread(of threadID: String) -> String? {
        Self.parent(of: threadID, items: items, byID: indexByAnyID)
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
        var children: [String: [String]] = [:]
        for thread in replies.keys {
            if let parent = Self.parent(of: thread, items: items, byID: byID) {
                children[parent, default: []].append(thread)
            }
        }
        childThreads = children
    }

    /// A thread's parent is the thread its root lives in, read from the
    /// root itself: the `parent` a sender writes is not trusted, so a
    /// claim cannot move messages under a thread they are not in. The root
    /// must be named by an id others can target (in a room, the room's
    /// stanza id). A thread whose parents loop back to it has none.
    private static func parent(of thread: String, items: [TimelineItem], byID: [String: Int]) -> String? {
        func link(_ thread: String) -> String? {
            guard let index = byID[thread] else { return nil }
            let root = items[index]
            return root.targetableIDs.contains(thread) ? root.containingThreadID : nil
        }
        guard let parent = link(thread) else { return nil }
        var seen: Set<String> = [thread]
        var cursor: String? = parent
        while let ancestor = cursor, seen.insert(ancestor).inserted {
            cursor = link(ancestor)
        }
        return cursor == thread ? nil : parent
    }
}
