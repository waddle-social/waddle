import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

/// A room message in `thread`, hanging off `parent` when it is a
/// sub-thread message.
private func message(
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

/// A room with a thread "root" (replies r1, r2) and a sub-thread on r1
/// (s1, s2), ingested out of order.
@MainActor
private func roomWithSubThread() -> ConversationTimeline {
    let store = TimelineStore()
    store.account = me
    store.ingest(message("s2", from: "erin", id: "s2", at: 5, thread: "r1", parent: "root"))
    store.ingest(message("root", from: "bob", id: "root", at: 1))
    store.ingest(message("r1", from: "carol", id: "r1", at: 2, thread: "root"))
    store.ingest(message("s1", from: "dave", id: "s1", at: 4, thread: "r1", parent: "root"))
    store.ingest(message("r2", from: "carol", id: "r2", at: 3, thread: "root"))
    return store.timeline(for: roomConversation)
}

@MainActor
@Suite("Sub-threads")
struct SubThreadTests {
    // MARK: Timeline

    @Test func subThreadMessagesStayInTheirOwnThread() {
        let timeline = roomWithSubThread()
        #expect(timeline.feedItems.map(\.body) == ["root"])
        #expect(timeline.threadReplies(threadID: "root").map(\.body) == ["r1", "r2"])
        #expect(timeline.threadReplies(threadID: "r1").map(\.body) == ["s1", "s2"])
    }

    @Test func aRootCountsItsSubThreadsOnce() throws {
        let timeline = roomWithSubThread()
        let root = try #require(timeline.item(withID: "root"))
        #expect(timeline.replyCount(for: root) == 4)
        let summary = try #require(timeline.threadSummary(for: root))
        #expect(summary.replyCount == 4)
        #expect(summary.latestReply?.snippet == .text("s2"))
        #expect(summary.faces.map(\.name) == ["erin", "dave", "carol"])
    }

    @Test func aReplyKnowsItsSubThread() throws {
        let timeline = roomWithSubThread()
        let r1 = try #require(timeline.item(withID: "r1"))
        let r2 = try #require(timeline.item(withID: "r2"))
        #expect(timeline.replyCount(for: r1) == 2)
        #expect(timeline.threadSummary(for: r1)?.latestReply?.snippet == .text("s2"))
        #expect(timeline.replyCount(for: r2) == 0)
        #expect(timeline.threadSummary(for: r2) == nil)
        #expect(timeline.parentThread(of: "r1") == "root")
        // A sub-thread not started yet already knows where it hangs off.
        #expect(timeline.parentThread(of: "r2") == "root")
        #expect(timeline.parentThread(of: "root") == nil)
    }

    @Test func nestedSubThreadsRollUpToEveryAncestor() throws {
        let store = TimelineStore()
        store.account = me
        store.ingest(message("root", from: "bob", id: "root", at: 1))
        store.ingest(message("r1", id: "r1", at: 2, thread: "root"))
        store.ingest(message("s1", id: "s1", at: 3, thread: "r1", parent: "root"))
        store.ingest(message("t1", id: "t1", at: 4, thread: "s1", parent: "r1"))
        let timeline = store.timeline(for: roomConversation)
        #expect(timeline.replyCount(for: try #require(timeline.item(withID: "root"))) == 3)
        #expect(timeline.replyCount(for: try #require(timeline.item(withID: "r1"))) == 2)
        #expect(timeline.replyCount(for: try #require(timeline.item(withID: "s1"))) == 1)
        #expect(timeline.parentThread(of: "s1") == "r1")
    }

    @Test func theParentIsWhereTheRootLivesNotWhatASenderClaims() throws {
        let store = TimelineStore()
        store.account = me
        store.ingest(message("root", from: "bob", id: "root", at: 1))
        store.ingest(message("other", from: "bob", id: "other", at: 2))
        store.ingest(message("r1", id: "r1", at: 3, thread: "root"))
        // Claims the wrong parent: it still belongs under r1, in "root".
        store.ingest(message("s1", id: "s1", at: 4, thread: "r1", parent: "other"))
        // A top-level thread claiming a parent stays top-level.
        store.ingest(message("o1", id: "o1", at: 5, thread: "other", parent: "root"))
        // A sub-thread whose root is not loaded links to nothing.
        store.ingest(message("g1", id: "g1", at: 6, thread: "ghost", parent: "root"))
        let timeline = store.timeline(for: roomConversation)
        let root = try #require(timeline.item(withID: "root"))
        let other = try #require(timeline.item(withID: "other"))
        #expect(timeline.replyCount(for: root) == 2)
        #expect(timeline.replyCount(for: other) == 1)
        #expect(timeline.parentThread(of: "r1") == "root")
        #expect(timeline.parentThread(of: "other") == nil)
        #expect(timeline.parentThread(of: "ghost") == nil)
    }

    @Test func aSubThreadOnARowWithoutARoomIDLinksToNothing() throws {
        let store = TimelineStore()
        store.account = me
        store.ingest(message("root", from: "bob", id: "root", at: 1))
        // A sender-chosen id is not one others can thread on in a room.
        var forged = roomMessage("r1", from: "carol", stanzaID: "r1-room", originID: "r1-origin", at: date(2))
        forged.thread = "root"
        store.ingest(forged)
        store.ingest(message("s1", id: "s1", at: 3, thread: "r1-origin", parent: "root"))
        let timeline = store.timeline(for: roomConversation)
        #expect(timeline.replyCount(for: try #require(timeline.item(withID: "root"))) == 1)
        #expect(timeline.parentThread(of: "r1-origin") == nil)
    }

    @Test func aCycleOfParentsLinksNothingAndStillCounts() throws {
        let store = TimelineStore()
        store.account = me
        let peer = jid("bob@waddle.test")
        let own = jid("alice@waddle.test")
        // In 1:1 ids are sender-chosen: two replies can each claim to live
        // in a thread rooted at the other.
        var a = directMessage("a", from: peer, to: own, id: "a", at: date(1))
        a.thread = "b"
        var b = directMessage("b", from: peer, to: own, id: "b", at: date(2))
        b.thread = "a"
        store.ingest(a)
        store.ingest(b)
        let timeline = store.timeline(for: bobConversation)
        #expect(timeline.parentThread(of: "a") == nil)
        #expect(timeline.parentThread(of: "b") == nil)
        #expect(timeline.replyCount(for: try #require(timeline.item(withID: "a"))) == 1)
        #expect(timeline.replyCount(for: try #require(timeline.item(withID: "b"))) == 1)
    }

    // MARK: Where "Reply in thread" goes

    @Test func replyInThreadFromTheFeedJoinsOrStartsTheRowsThread() throws {
        let timeline = roomWithSubThread()
        let root = try #require(timeline.item(withID: "root"))
        #expect(root.threadTarget(inside: nil) == "root")
    }

    @Test func replyInThreadInsideAThreadStartsASubThreadOnAReply() throws {
        let timeline = roomWithSubThread()
        let root = try #require(timeline.item(withID: "root"))
        let r2 = try #require(timeline.item(withID: "r2"))
        let s1 = try #require(timeline.item(withID: "s1"))
        #expect(r2.threadTarget(inside: "root") == "r2")
        // The open thread's own root offers nothing.
        #expect(root.threadTarget(inside: "root") == nil)
        // Inside the sub-thread, its root (a reply in "root") offers nothing.
        let r1 = try #require(timeline.item(withID: "r1"))
        #expect(r1.threadTarget(inside: "r1") == nil)
        #expect(s1.threadTarget(inside: "r1") == "s1")
    }

    // MARK: Fetched history

    @Test func aSubThreadsFetchedPageHasItsRootAndParent() {
        let page = ArchivePage(messages: [
            message("r1", id: "r1", at: 2, thread: "root"),
            message("s1", id: "s1", at: 4, thread: "r1", parent: "root"),
            message("s2", id: "s2", at: 5, thread: "r1", parent: "root"),
        ], first: "r1", isComplete: true)
        let history = ThreadHistory.build(from: page, thread: ThreadKey(room: room, threadID: "r1"), account: me)
        #expect(history.root?.body == "r1")
        #expect(history.root?.containingThreadID == "root")
        #expect(history.replies.map(\.body) == ["s1", "s2"])
    }

    @Test func aParentsFetchedPageKeepsSubThreadMessagesOutOfItsReplies() {
        let page = ArchivePage(messages: [
            message("root", from: "bob", id: "root", at: 1),
            message("r1", id: "r1", at: 2, thread: "root"),
            message("s1", id: "s1", at: 4, thread: "r1", parent: "root"),
        ], first: "root", isComplete: true)
        let history = ThreadHistory.build(from: page, thread: ThreadKey(room: room, threadID: "root"), account: me)
        #expect(history.root?.body == "root")
        #expect(history.root?.containingThreadID == nil)
        #expect(history.replies.map(\.body) == ["r1"])
    }

    /// A thread opened from the Inbox has replies the room has not
    /// loaded; a sub-thread started on one still knows its parent before
    /// its own page is fetched.
    @Test func aFetchedThreadTellsWhereASubThreadOnItsReplyHangs() {
        let page = ArchivePage(messages: [
            message("root", from: "bob", id: "root", at: 1),
            message("r1", id: "r1", at: 2, thread: "root"),
        ], first: "root", isComplete: true)
        let parentKey = ThreadKey(room: room, threadID: "root")
        let coordinator = SessionCoordinator(account: me, port: FakePort())
        coordinator.threadHistory.store(ThreadHistory.build(from: page, thread: parentKey, account: me), for: parentKey)
        #expect(coordinator.threadParent(of: "r1", in: roomConversation) == "root")
        #expect(coordinator.threadParent(of: "root", in: roomConversation) == nil)
        #expect(coordinator.threadParent(of: "r1", in: .room(bare("other@muc.waddle.test"))) == nil)
    }

    // MARK: Inbox

    @Test func anUnreadSubThreadListsItsOwnReplies() {
        let page = ArchivePage(messages: [
            message("r1", id: "r1", at: 2, thread: "root"),
            message("s1", from: "dave", id: "s1", at: 4, thread: "r1", parent: "root"),
        ], first: "r1", isComplete: true)
        let replies = UnreadOverview.threadMessages(
            from: page,
            thread: ThreadKey(room: room, threadID: "r1"),
            unread: 5,
            account: me
        )
        #expect(replies.map(\.body) == ["s1"])
    }
}
