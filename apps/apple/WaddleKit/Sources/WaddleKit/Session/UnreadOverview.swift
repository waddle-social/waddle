import Foundation

/// A room with unread activity, as the Activity overview shows it: its
/// unread feed messages, then each unread thread with its unread replies.
/// The native counterpart of the web client's unread view.
public struct UnreadOverviewGroup: Identifiable, Equatable, Sendable {
    public let room: BareJID
    public var title: String
    /// Server inbox count for the room row. It also counts thread replies
    /// and reactions, so it can exceed `messages.count`.
    public var unread: Int
    public var mentionsMe: Bool
    /// Waddle `last-updated`, epoch seconds: newest of the room and thread rows.
    public var lastUpdated: Int64?
    public var messages: [TimelineItem]
    public var threads: [UnreadOverviewThread]
    /// A section's fetch failed; its messages may be missing.
    public var isIncomplete: Bool

    public var id: BareJID { room }
    public var conversation: ConversationID { .room(room) }
}

public struct UnreadOverviewThread: Identifiable, Equatable, Sendable {
    public let key: ThreadKey
    public var title: String
    public var unread: Int
    public var lastUpdated: Int64?
    public var messages: [TimelineItem]

    public var id: ThreadKey { key }
}

/// What the overview should show before any message is fetched.
struct UnreadOverviewCandidate: Equatable {
    struct Thread: Equatable {
        let key: ThreadKey
        let title: String
        let unread: Int
        let lastUpdated: Int64?
        let lastStanzaID: String?
    }

    let room: BareJID
    let unread: Int
    let lastUpdated: Int64?
    let lastStanzaID: String?
    let threads: [Thread]
}

enum UnreadOverview {
    /// Web parity: fetch the unread count plus some context, capped.
    static func fetchSize(for unread: Int) -> Int {
        min(100, unread + 20)
    }

    /// Rooms with an unread room row or an unread thread, newest first
    /// (ties by room JID); threads newest first. Only rooms the directory
    /// knows: inbox rows outlive a room the account left.
    static func candidates(
        counts: [ConversationID: Int],
        threadCounts: [ThreadKey: Int],
        entry: (BareJID, String?) -> InboxEntry?,
        isRoom: (BareJID) -> Bool,
        threadTitle: (ThreadKey, InboxEntry?) -> String
    ) -> [UnreadOverviewCandidate] {
        var threadsByRoom: [BareJID: [UnreadOverviewCandidate.Thread]] = [:]
        for (key, unread) in threadCounts where unread > 0 && isRoom(key.room) {
            let row = entry(key.room, key.threadID)
            threadsByRoom[key.room, default: []].append(.init(
                key: key,
                title: threadTitle(key, row),
                unread: unread,
                lastUpdated: row?.lastUpdated,
                lastStanzaID: row?.lastStanzaID
            ))
        }
        var rooms = Set(threadsByRoom.keys)
        for (conversation, unread) in counts where conversation.isRoom && unread > 0 && isRoom(conversation.jid) {
            rooms.insert(conversation.jid)
        }
        let candidates = rooms.map { room -> UnreadOverviewCandidate in
            let row = entry(room, nil)
            let threads = (threadsByRoom[room] ?? []).sorted { newer($0.lastUpdated, $0.key.threadID, than: $1.lastUpdated, $1.key.threadID) }
            let newest = ([row?.lastUpdated] + threads.map(\.lastUpdated)).compactMap { $0 }.max()
            return UnreadOverviewCandidate(
                room: room,
                unread: counts[.room(room)] ?? 0,
                lastUpdated: newest,
                lastStanzaID: row?.lastStanzaID,
                threads: threads
            )
        }
        return candidates.sorted { newer($0.lastUpdated, $0.room.description, than: $1.lastUpdated, $1.room.description) }
    }

    private static func newer(_ lhs: Int64?, _ lhsKey: String, than rhs: Int64?, _ rhsKey: String) -> Bool {
        let left = lhs ?? .min
        let right = rhs ?? .min
        return left != right ? left > right : lhsKey < rhsKey
    }

    /// Web `threadDisplayTitle`: the inbox thread title, else the row's
    /// preview, else a generic label.
    static func threadTitle(_ entry: InboxEntry?) -> String {
        for candidate in [entry?.threadTitle, entry?.preview] {
            let text = candidate?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
            if !text.isEmpty { return text }
        }
        return "Thread"
    }

    /// The room's unread feed messages from a newest-first page: others'
    /// visible rows after our XEP-0490 read cursor when the page reaches
    /// it, capped at the unread count (which also counts thread replies
    /// and reactions, so the cursor is the better bound).
    @MainActor
    static func roomMessages(
        from page: ArchivePage,
        room: BareJID,
        unread: Int,
        readCursor: String?,
        account: AccountIdentity
    ) -> [TimelineItem] {
        let timeline = scratchTimeline(page, conversation: .room(room), account: account)
        var rows = timeline.items.filter(\.isFeedVisible)
        if let readCursor, let index = rows.lastIndex(where: { $0.id == readCursor || $0.identity.all.contains(readCursor) }) {
            rows = Array(rows[(index + 1)...])
        }
        return Array(rows.filter(isUnreadCandidate).suffix(unread))
    }

    /// The thread's unread replies: others' replies carrying the thread,
    /// newest `unread` of them. The root and threadless replies the filter
    /// also returns are not counted by the thread row, so they are left out.
    @MainActor
    static func threadMessages(from page: ArchivePage, thread: ThreadKey, unread: Int, account: AccountIdentity) -> [TimelineItem] {
        let timeline = scratchTimeline(page, conversation: .room(thread.room), account: account)
        let replies = timeline.items.filter { $0.message.thread == thread.threadID && $0.id != thread.threadID }
        return Array(replies.filter(isUnreadCandidate).suffix(unread))
    }

    private static func isUnreadCandidate(_ item: TimelineItem) -> Bool {
        !item.isMine && item.tombstone == nil && !item.isLocalEcho
    }

    @MainActor
    private static func scratchTimeline(_ page: ArchivePage, conversation: ConversationID, account: AccountIdentity) -> ConversationTimeline {
        let scratch = TimelineStore()
        scratch.account = account
        for message in page.messages {
            scratch.ingest(message)
        }
        return scratch.timeline(for: conversation)
    }
}
