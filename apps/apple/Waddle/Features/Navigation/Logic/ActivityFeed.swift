import Foundation
import WaddleKit

/// What the Activity overview depends on: room and thread unread counts
/// and whether the session can fetch. A change re-runs its refresh.
struct ActivityRefreshKey: Hashable {
    let rooms: [ConversationID: Int]
    let threads: [ThreadKey: Int]
    let isOnline: Bool

    @MainActor
    init(session: SessionCoordinator) {
        rooms = session.unread.counts.filter { $0.key.isRoom }
        threads = session.unread.threadCounts
        isOnline = session.connection == .online
    }
}

/// VoiceOver copy for Activity rows.
enum ActivityCopy {
    static func groupLabel(_ group: UnreadOverviewGroup) -> String {
        var parts = [group.title]
        if group.unread > 0 {
            parts.append(group.unread == 1 ? "1 unread" : "\(group.unread) unread")
        }
        if group.mentionsMe {
            parts.append("mentions you")
        }
        let threads = group.threads.count
        if threads > 0 {
            parts.append(threads == 1 ? "1 thread with replies" : "\(threads) threads with replies")
        }
        return parts.joined(separator: ", ")
    }

    static func threadLabel(_ thread: UnreadOverviewThread) -> String {
        let replies = thread.unread == 1 ? "1 unread reply" : "\(thread.unread) unread replies"
        return "Thread \(thread.title), \(replies)"
    }

    static func messageLabel(_ item: TimelineItem, isThreadReply: Bool) -> String {
        let content = RowPreview.content(of: item) ?? ""
        let time = ListTimestamp.string(for: item.sentAt)
        let prefix = isThreadReply ? "Reply from" : "From"
        return "\(prefix) \(item.authorName), \(time): \(content)"
    }
}
