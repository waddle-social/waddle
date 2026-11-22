import Foundation

/// One thread in a sub-thread's breadcrumb.
public struct ThreadCrumb: Hashable, Sendable {
    public let threadID: String
    /// The thread's first message as one line; nil when it is not loaded.
    public let title: ThreadSummary.Snippet?

    public init(threadID: String, title: ThreadSummary.Snippet?) {
        self.threadID = threadID
        self.title = title
    }
}

/// What the thread screen's header shows, as web's thread panel header
/// does: the thread's first message as its title, "Started by <name> ·
/// N replies · active <when>", the participants' faces, and for a
/// sub-thread the breadcrumb of threads it hangs under.
public struct ThreadHeader: Equatable, Sendable {
    /// Faces shown before the overflow count, as on web.
    public static let maxFaces = 4

    /// The root as one line; nil when it is not loaded.
    public let title: ThreadSummary.Snippet?
    /// Who wrote the root.
    public let startedBy: String?
    /// The thread's messages, its sub-threads' included.
    public let replyCount: Int
    /// The newest direct reply, else the root.
    public let lastActivity: Date?
    /// Newest reply's author first; the root's author last.
    public let faces: [ThreadSummary.Participant]
    public let otherParticipantCount: Int
    /// From the top-level thread down to this one.
    public let breadcrumb: [ThreadCrumb]

    /// A sub-thread shows its breadcrumb instead of a plain title.
    public var isSubThread: Bool { breadcrumb.count > 1 }

    /// `replies` are the thread's direct replies, oldest first, which give
    /// the faces and recency as on web; `replyCount` is the rolled-up
    /// count shown under the root.
    public init(root: TimelineItem?, replies: [TimelineItem], replyCount: Int, breadcrumb: [ThreadCrumb]) {
        title = root.map(ThreadSummary.snippet(of:))
        startedBy = root.map(\.authorName).flatMap { $0.isEmpty ? nil : $0 }
        self.replyCount = replyCount
        let everyone = ThreadSummary(replyCount: replyCount, replies: (root.map { [$0] } ?? []) + replies, maxFaces: Self.maxFaces)
        lastActivity = everyone.latestReply?.sentAt
        faces = everyone.faces
        otherParticipantCount = everyone.otherParticipantCount
        self.breadcrumb = breadcrumb
    }

    /// Web's "active …" wording: "just now", "N min ago", "N hours ago",
    /// "yesterday", "N days ago". Nil for a time after `now`.
    public static func activity(since date: Date, now: Date) -> String? {
        let seconds = now.timeIntervalSince(date)
        guard seconds >= 0 else { return nil }
        if seconds < 45 { return "just now" }
        let minutes = Int(seconds / 60)
        if minutes < 2 { return "1 min ago" }
        if minutes < 60 { return "\(minutes) min ago" }
        let hours = minutes / 60
        if hours < 2 { return "1 hour ago" }
        if hours < 24 { return "\(hours) hours ago" }
        let days = hours / 24
        return days < 2 ? "yesterday" : "\(days) days ago"
    }
}
