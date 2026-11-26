import Foundation

extension SessionCoordinator {
    /// The thread `threadID` hangs off, as the thread screen shows its way
    /// back and as a send in it names its XEP-0201 `parent`; nil for a
    /// top-level thread.
    ///
    /// Where the sub-thread's root lives decides: the conversation's
    /// timeline (which also links and counts sub-threads by this rule),
    /// else a fetched thread page. Without the root (a sub-thread opened
    /// from the Inbox before its page arrives, or one longer than a page),
    /// it is the `parent` most of the sub-thread's own messages carry,
    /// the oldest one's on a tie.
    public func threadParent(of threadID: String, in conversation: ConversationID) -> String? {
        let timeline = timelines.timeline(for: conversation)
        if let root = timeline.item(withID: threadID), root.targetableIDs.contains(threadID) {
            return timeline.parentThread(of: threadID)
        }
        let key = conversation.isRoom ? ThreadKey(room: conversation.jid, threadID: threadID) : nil
        if let key, let root = threadHistory.fetchedRoot(of: key) {
            return root.containingThreadID
        }
        let fetched = key.flatMap { threadHistory.history(for: $0)?.replies } ?? []
        let messages = ThreadHistory.merged(live: timeline.threadReplies(threadID: threadID), fetched: fetched)
        return Self.claimedParent(of: threadID, in: messages)
    }

    /// The first message of the thread `threadID`: the row the id names
    /// (a thread started from a message uses its id) in the timeline or in
    /// a fetched thread page, else the first feed row carrying that
    /// XEP-0201 thread.
    public func threadRoot(of threadID: String, in conversation: ConversationID) -> TimelineItem? {
        let timeline = timelines.timeline(for: conversation)
        if let root = timeline.item(withID: threadID) {
            return root
        }
        // A call's announcement heads its thread without sharing its id. The
        // feed's own copy is preferred to a fetched one: the room's word that
        // the call ended is no part of the thread, so only the feed's has it.
        if let anchor = timeline.items.first(where: { $0.callAnchor != nil && $0.message.thread == threadID }) {
            return anchor
        }
        if conversation.isRoom, let root = threadHistory.fetchedRoot(of: ThreadKey(room: conversation.jid, threadID: threadID)) {
            return root
        }
        return timeline.items.first { $0.isFeedVisible && $0.message.thread == threadID }
    }

    /// The threads `threadID` hangs under, from the top-level one down to
    /// it, each with its first message as a title.
    public func threadBreadcrumb(of threadID: String, in conversation: ConversationID) -> [ThreadCrumb] {
        var chain = [threadID]
        var seen: Set = [threadID]
        var cursor = threadParent(of: threadID, in: conversation)
        while let parent = cursor, seen.insert(parent).inserted {
            chain.insert(parent, at: 0)
            cursor = threadParent(of: parent, in: conversation)
        }
        return chain.map { id in
            ThreadCrumb(threadID: id, title: threadRoot(of: id, in: conversation).map(ThreadSummary.snippet(of:)))
        }
    }

    /// `message` with its sub-thread's parent filled in when it has none
    /// and the parent is known.
    func withThreadParent(_ message: OutboundMessage) -> OutboundMessage {
        guard let thread = message.options.thread, thread.parent == nil,
              let parent = threadParent(of: thread.id, in: message.conversation)
        else { return message }
        var options = message.options
        options.thread?.parent = parent
        return OutboundMessage(clientID: message.clientID, conversation: message.conversation, body: message.body, options: options)
    }

    /// The `parent` most of `messages` (oldest first) carry, the earliest
    /// one on a tie. A message naming the thread itself counts for none.
    static func claimedParent(of threadID: String, in messages: [TimelineItem]) -> String? {
        var counts: [String: Int] = [:]
        var order: [String] = []
        for message in messages {
            guard let parent = message.message.parentThread, parent != threadID else { continue }
            if counts[parent] == nil { order.append(parent) }
            counts[parent, default: 0] += 1
        }
        return order.max { (counts[$0] ?? 0) < (counts[$1] ?? 0) }
    }
}
