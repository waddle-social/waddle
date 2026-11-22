import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

private func archived(
    _ body: String,
    from nick: String = "carol",
    id: String,
    at seconds: TimeInterval,
    thread: String? = nil,
    parent: String? = nil
) -> WireMessage {
    var message = roomMessage(body, from: nick, stanzaID: id, at: date(seconds), source: .archive(mamID: id))
    message.thread = thread
    message.parentThread = parent
    return message
}

/// The thread screen's header, as web's thread panel header: the root as
/// its title, "Started by · N replies · active …", faces, and a
/// breadcrumb for a sub-thread.
@MainActor
@Suite("Thread header")
struct ThreadHeaderTests {
    private func session() -> SessionCoordinator {
        let coordinator = SessionCoordinator(account: me, port: FakePort())
        coordinator.ingestArchive(ArchivePage(messages: [
            archived("Shall we  move\nthe release?", from: "priya", id: "root", at: 1),
            archived("Thursday works", from: "jo", id: "r1", at: 2, thread: "root"),
            archived("Android needs a day", from: "mika", id: "r2", at: 3, thread: "root"),
            archived("I can pair", from: "theo", id: "s1", at: 4, thread: "r2", parent: "root"),
            archived("Deeper", from: "erin", id: "t1", at: 5, thread: "s1", parent: "r2"),
        ], first: "root", isComplete: true))
        return coordinator
    }

    @Test func aTopLevelThreadsHeader() throws {
        let coordinator = session()
        let timeline = coordinator.timelines.timeline(for: roomConversation)
        let header = ThreadHeader(
            root: coordinator.threadRoot(of: "root", in: roomConversation),
            replies: timeline.threadReplies(threadID: "root"),
            replyCount: 4,
            breadcrumb: coordinator.threadBreadcrumb(of: "root", in: roomConversation)
        )
        #expect(header.title == .text("Shall we move the release?"))
        #expect(header.startedBy == "priya")
        #expect(header.replyCount == 4)
        #expect(header.lastActivity == date(3))
        // Newest reply first, then the thread's starter.
        #expect(header.faces.map(\.name) == ["mika", "jo", "priya"])
        #expect(header.otherParticipantCount == 0)
        #expect(header.breadcrumb.map(\.threadID) == ["root"])
        #expect(!header.isSubThread)
    }

    @Test func aSubThreadsBreadcrumbRunsFromTheTopThread() {
        let coordinator = session()
        let crumbs = coordinator.threadBreadcrumb(of: "s1", in: roomConversation)
        #expect(crumbs.map(\.threadID) == ["root", "r2", "s1"])
        #expect(crumbs.map(\.title) == [.text("Shall we move the release?"), .text("Android needs a day"), .text("I can pair")])
    }

    @Test func aSubThreadsHeaderIsItsReply() {
        let coordinator = session()
        let timeline = coordinator.timelines.timeline(for: roomConversation)
        let header = ThreadHeader(
            root: coordinator.threadRoot(of: "r2", in: roomConversation),
            replies: timeline.threadReplies(threadID: "r2"),
            replyCount: 2,
            breadcrumb: coordinator.threadBreadcrumb(of: "r2", in: roomConversation)
        )
        #expect(header.title == .text("Android needs a day"))
        #expect(header.startedBy == "mika")
        #expect(header.isSubThread)
        #expect(header.breadcrumb.map(\.threadID) == ["root", "r2"])
        #expect(header.lastActivity == date(4))
    }

    @Test func aThreadWhoseRootIsNotLoaded() {
        let coordinator = SessionCoordinator(account: me, port: FakePort())
        let header = ThreadHeader(
            root: coordinator.threadRoot(of: "ghost", in: roomConversation),
            replies: [],
            replyCount: 0,
            breadcrumb: coordinator.threadBreadcrumb(of: "ghost", in: roomConversation)
        )
        #expect(header.title == nil)
        #expect(header.startedBy == nil)
        #expect(header.lastActivity == nil)
        #expect(header.faces.isEmpty)
        #expect(header.breadcrumb == [ThreadCrumb(threadID: "ghost", title: nil)])
    }

    @Test func atMostFourFacesWithTheRestCounted() {
        let replies = ["a", "b", "c", "d", "e"].enumerated().map { index, nick in
            TimelineItem(
                id: nick,
                conversation: roomConversation,
                isMine: false,
                message: archived(nick, from: nick, id: nick, at: TimeInterval(index + 2), thread: "root"),
                body: nick
            )
        }
        let header = ThreadHeader(root: nil, replies: replies, replyCount: 5, breadcrumb: [])
        #expect(header.faces.map(\.name) == ["e", "d", "c", "b"])
        #expect(header.otherParticipantCount == 1)
    }

    @Test func recencyReadsLikeWebs() {
        let now = date(1_000_000)
        func label(_ secondsAgo: TimeInterval) -> String? {
            ThreadHeader.activity(since: now.addingTimeInterval(-secondsAgo), now: now)
        }
        #expect(label(10) == "just now")
        #expect(label(70) == "1 min ago")
        #expect(label(30 * 60) == "30 min ago")
        #expect(label(90 * 60) == "1 hour ago")
        #expect(label(5 * 3600) == "5 hours ago")
        #expect(label(30 * 3600) == "yesterday")
        #expect(label(4 * 86400) == "4 days ago")
        #expect(label(-60) == nil)
    }
}
