import Foundation
import WaddleKit

/// Where the "Unread" divider goes and how many rows arrived below the
/// viewport. Only others' content rows count, matching the unread badge.
enum TimelineUnreadAnchor {
    /// The first unread row: `unreadCount` counted rows from the end. When
    /// fewer rows are loaded than are unread, the oldest counted row.
    static func firstUnreadID(in items: [TimelineItem], unreadCount: Int) -> String? {
        guard unreadCount > 0 else { return nil }
        var remaining = unreadCount
        var oldest: String?
        for item in items.reversed() where counts(item) {
            oldest = item.id
            remaining -= 1
            if remaining == 0 { return item.id }
        }
        return oldest
    }

    /// Counted rows after the row `previousLastID`. Zero when that row is
    /// no longer loaded (the feed was replaced, not appended to).
    static func arrivals(after previousLastID: String?, in items: [TimelineItem]) -> Int {
        guard let previousLastID, let index = items.lastIndex(where: { $0.id == previousLastID }) else {
            return 0
        }
        return items[(index + 1)...].filter(counts).count
    }

    private static func counts(_ item: TimelineItem) -> Bool {
        !item.isMine && item.tombstone == nil
    }
}
