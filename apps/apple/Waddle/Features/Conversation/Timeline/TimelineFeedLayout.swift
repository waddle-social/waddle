import Foundation
import WaddleKit

/// One feed row with everything the row view needs precomputed, so row
/// bodies never scan the timeline.
struct TimelineFeedEntry: Identifiable, Hashable {
    var id: String { item.presentationID }
    let item: TimelineItem
    /// Start of the day when a day separator precedes this row.
    let daySeparator: Date?
    /// The "Unread" divider precedes this row, and which side of it the
    /// unread rows are on.
    let unreadDivider: TimelineUnreadDivider.Edge?
    /// First row of an author run: shows the avatar and name header.
    let startsGroup: Bool
    /// XEP-0201 replies in the thread rooted at this row.
    let replyCount: Int
    /// The XEP-0461 parent, when it is loaded.
    let replyParent: TimelineItem?
}

/// Derives day separators, author grouping and the unread divider from the
/// feed. Pure, so it runs once per timeline change.
///
/// `items` are oldest first. With `newestFirst` the entries come out
/// newest first, as the social timeline shows them: each day's separator
/// heads its newest row, an author run's header sits on its newest row,
/// and the unread divider goes under the oldest unread row, since the
/// unread rows are the ones above it.
enum TimelineFeedLayout {
    static func entries(
        for items: [TimelineItem],
        unreadAnchorID: String?,
        groupingWindow: TimeInterval,
        newestFirst: Bool = false,
        calendar: Calendar = .current,
        replyCount: (TimelineItem) -> Int = { _ in 0 },
        replyParent: (String) -> TimelineItem? = { _ in nil }
    ) -> [TimelineFeedEntry] {
        let ordered: [TimelineItem] = newestFirst ? items.reversed() : items
        let dividerID = unreadAnchorID.flatMap { anchor in
            newestFirst ? rowAfter(anchor, in: ordered) : anchor
        }
        let edge: TimelineUnreadDivider.Edge = newestFirst ? .unreadAbove : .unreadBelow
        var entries: [TimelineFeedEntry] = []
        entries.reserveCapacity(ordered.count)
        var previous: TimelineItem?
        for item in ordered {
            let separator = daySeparator(before: item, previous: previous, calendar: calendar)
            let unread = item.presentationID == dividerID
            let starts = separator != nil || unread
                || !continuesGroup(item, after: previous, window: groupingWindow)
            entries.append(TimelineFeedEntry(
                item: item,
                daySeparator: separator,
                unreadDivider: unread ? edge : nil,
                startsGroup: starts,
                replyCount: replyCount(item),
                replyParent: item.message.reply.flatMap { replyParent($0.id) }
            ))
            previous = item
        }
        return entries
    }

    /// The row after `id`: in a newest-first feed, the newest row that was
    /// already read. Nil when `id` is the last row loaded, so every loaded
    /// row is unread and there is nothing to divide.
    private static func rowAfter(_ id: String, in items: [TimelineItem]) -> String? {
        guard let index = items.firstIndex(where: { $0.presentationID == id }), index + 1 < items.count else { return nil }
        return items[index + 1].presentationID
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
