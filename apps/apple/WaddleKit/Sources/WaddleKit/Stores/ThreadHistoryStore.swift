import Foundation
import Observation

/// One room thread as fetched with the Waddle MAM thread filter: its root
/// (the filter includes it) and its replies, oldest first. The filter
/// matches the thread id only, so a page holds no sub-thread messages;
/// should one arrive, it stays out of `replies`.
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
    /// matched by room stanza ID, by the room archive's ID, or by an
    /// unambiguous verified author alias.
    public static func merged(live: [TimelineItem], fetched: [TimelineItem]) -> [TimelineItem] {
        let missing = fetched.indices.compactMap { index -> TimelineItem? in
            let item = fetched[index]
            if item.roomStanzaMatch(in: live) != nil { return nil }
            if item.roomArchiveMatch(in: live) != nil { return nil }

            guard let liveIndex = item.roomAuthoredMatch(in: live) else { return item }
            let liveItem = live[liveIndex]
            // A reused alias must select a single counterpart in both lists.
            let fetchedMatch = liveItem.roomStanzaMatch(in: fetched) ?? liveItem.roomAuthoredMatch(in: fetched)
            return fetchedMatch == index ? nil : item
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

    public func history(for thread: ThreadKey) -> ThreadHistory? {
        histories[thread]
    }

    /// The root of `thread` as fetched: its own page's root, else a
    /// fetched thread of the room with that root among its replies (a
    /// sub-thread on a reply only a fetched page holds).
    public func fetchedRoot(of thread: ThreadKey) -> TimelineItem? {
        if let root = histories[thread]?.root {
            return root
        }
        for (key, history) in histories where key.room == thread.room {
            if let reply = history.replies.first(where: { $0.targetableIDs.contains(thread.threadID) }) {
                return reply
            }
        }
        return nil
    }

    func store(_ history: ThreadHistory, for thread: ThreadKey) {
        histories[thread] = history
    }

    func clear() {
        histories.removeAll()
    }
}
