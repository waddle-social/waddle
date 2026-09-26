import Foundation
import Observation

/// One room thread as fetched with the Waddle MAM thread filter: its root
/// (the filter includes it) and its replies, oldest first.
public struct ThreadHistory: Equatable, Sendable {
    public var root: TimelineItem?
    public var replies: [TimelineItem]

    public init(root: TimelineItem?, replies: [TimelineItem]) {
        self.root = root
        self.replies = replies
    }

    /// Reduces a thread page through a scratch timeline, so corrections,
    /// retractions and reactions inside the page apply to their targets.
    @MainActor
    public static func build(from page: ArchivePage, thread: ThreadKey, account: AccountIdentity) -> ThreadHistory {
        let scratch = TimelineStore()
        scratch.account = account
        for message in page.messages {
            scratch.ingest(message)
        }
        let timeline = scratch.timeline(for: .room(thread.room))
        let root = timeline.item(withID: thread.threadID)
            ?? timeline.items.first { $0.isFeedVisible && $0.message.thread == thread.threadID }
        return ThreadHistory(root: root, replies: timeline.threadReplies(threadID: thread.threadID))
    }

    /// Live replies from the conversation timeline plus fetched ones it
    /// does not have, oldest first. A live row wins over its fetched copy,
    /// matched by any of its ids.
    public static func merged(live: [TimelineItem], fetched: [TimelineItem]) -> [TimelineItem] {
        var known = Set<String>()
        for item in live {
            known.insert(item.id)
            known.formUnion(item.identity.all)
        }
        let missing = fetched.filter { item in
            !known.contains(item.id) && known.isDisjoint(with: item.identity.all)
        }
        guard !missing.isEmpty else { return live }
        return (live + missing).enumerated()
            .sorted { lhs, rhs in
                lhs.element.sentAt == rhs.element.sentAt
                    ? lhs.offset < rhs.offset
                    : lhs.element.sentAt < rhs.element.sentAt
            }
            .map(\.element)
    }
}

/// Fetched thread histories, by thread. Kept apart from `TimelineStore`:
/// a thread page reaches arbitrarily far back, and mixing it into the
/// conversation timeline would move that timeline's paging cursor and read
/// cursor onto rows outside its loaded window.
@MainActor
@Observable
public final class ThreadHistoryStore {
    public private(set) var histories: [ThreadKey: ThreadHistory] = [:]

    public init() {}

    public func history(for thread: ThreadKey) -> ThreadHistory? {
        histories[thread]
    }

    func store(_ history: ThreadHistory, for thread: ThreadKey) {
        histories[thread] = history
    }

    func clear() {
        histories.removeAll()
    }
}
