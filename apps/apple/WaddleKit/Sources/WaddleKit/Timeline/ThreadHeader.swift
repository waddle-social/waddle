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
/// does: the thread's first message as its title, who started it, the
/// reply count, when it last moved, the participants' faces, and for a
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

    /// The thread a sub-thread hangs off.
    public var parent: ThreadCrumb? {
        breadcrumb.count > 1 ? breadcrumb[breadcrumb.count - 2] : nil
    }

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
}
