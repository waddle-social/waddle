import Foundation
import WaddleKit

/// One feed row with everything the row view needs precomputed, so row
/// bodies never scan the timeline.
struct TimelineFeedEntry: Identifiable, Hashable {
    var id: String { item.id }
    let item: TimelineItem
    /// Start of the day when a day separator precedes this row.
    let daySeparator: Date?
    /// The "Unread" divider precedes this row.
    let showsUnreadDivider: Bool
    /// First row of an author run: shows the avatar and name header.
    let startsGroup: Bool
    /// XEP-0201 replies in the thread rooted at this row.
    let replyCount: Int
    /// The XEP-0461 parent, when it is loaded.
    let replyParent: TimelineItem?
}

/// Derives day separators, author grouping and the unread divider from the
/// ordered feed. Pure, so it runs once per timeline change.
enum TimelineFeedLayout {
    static func entries(
        for items: [TimelineItem],
        unreadAnchorID: String?,
        groupingWindow: TimeInterval,
        calendar: Calendar = .current,
        replyCount: (TimelineItem) -> Int = { _ in 0 },
        replyParent: (String) -> TimelineItem? = { _ in nil }
    ) -> [TimelineFeedEntry] {
        var entries: [TimelineFeedEntry] = []
        entries.reserveCapacity(items.count)
        var previous: TimelineItem?
        for item in items {
            let separator = daySeparator(before: item, previous: previous, calendar: calendar)
            let unread = item.id == unreadAnchorID
            let starts = separator != nil || unread
                || !continuesGroup(item, after: previous, window: groupingWindow)
            entries.append(TimelineFeedEntry(
                item: item,
                daySeparator: separator,
                showsUnreadDivider: unread,
                startsGroup: starts,
                replyCount: replyCount(item),
                replyParent: item.message.reply.flatMap { replyParent($0.id) }
            ))
            previous = item
        }
        return entries
    }

    static func daySeparator(before item: TimelineItem, previous: TimelineItem?, calendar: Calendar) -> Date? {
        let day = calendar.startOfDay(for: item.sentAt)
        guard let previous else { return day }
        return calendar.isDate(previous.sentAt, inSameDayAs: item.sentAt) ? nil : day
    }

    /// Same author, close in time, and neither row is a tombstone.
    static func continuesGroup(_ item: TimelineItem, after previous: TimelineItem?, window: TimeInterval) -> Bool {
        guard let previous, !item.authorKey.isEmpty else { return false }
        guard previous.authorKey == item.authorKey else { return false }
        guard (previous.tombstone == nil) == (item.tombstone == nil) else { return false }
        return abs(item.sentAt.timeIntervalSince(previous.sentAt)) <= window
    }
}
