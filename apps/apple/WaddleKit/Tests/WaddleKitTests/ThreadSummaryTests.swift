import Foundation
import Testing
@testable import WaddleKit

@MainActor
@Suite("Thread summary")
struct ThreadSummaryTests {
    private func reply(
        _ body: String,
        from nick: String,
        id: String,
        at seconds: TimeInterval,
        realJID: BareJID? = nil,
        files: [SharedFile] = [],
        tombstone: Tombstone? = nil
    ) -> TimelineItem {
        var message = roomMessage(body, from: nick, stanzaID: id, at: date(seconds))
        message.thread = "root"
        message.authorRealJID = realJID
        message.sharedFiles = files
        return TimelineItem(
            id: id,
            conversation: roomConversation,
            isMine: false,
            message: message,
            body: body,
            tombstone: tombstone
        )
    }

    @Test func noLoadedRepliesHasNoSummary() {
        #expect(ThreadSummary.of(replies: []) == nil)
    }

    @Test func aKnownCountWithoutLoadedRepliesHasNoLatestReply() {
        let summary = ThreadSummary(replyCount: 4, replies: [])
        #expect(summary.replyCount == 4)
        #expect(summary.latestReply == nil)
        #expect(summary.faces.isEmpty)
        #expect(summary.otherParticipantCount == 0)
    }

    @Test func latestReplyIsTheNewestLoadedReply() throws {
        let dave = bare("dave@waddle.test")
        let summary = try #require(ThreadSummary.of(replies: [
            reply("first", from: "carol", id: "r1", at: 1),
            reply("Love this,\n\n  watching   now", from: "guzzle", id: "r2", at: 2, realJID: dave),
        ]))
        #expect(summary.replyCount == 2)
        let latest = try #require(summary.latestReply)
        #expect(latest.authorName == "guzzle")
        #expect(latest.authorJID == dave)
        #expect(latest.snippet == .text("Love this, watching now"))
        #expect(latest.sentAt == date(2))
    }

    @Test func facesAreDistinctAuthorsNewestFirstWithOverflow() throws {
        let dave = bare("dave@waddle.test")
        let summary = try #require(ThreadSummary.of(replies: [
            reply("a", from: "erin", id: "r1", at: 1),
            reply("b", from: "frank", id: "r2", at: 2),
            reply("c", from: "carol", id: "r3", at: 3),
            reply("d", from: "dave", id: "r4", at: 4, realJID: dave),
            // Same real JID under a new nick is the same person.
            reply("e", from: "dave2", id: "r5", at: 5, realJID: dave),
            reply("f", from: "carol", id: "r6", at: 6),
        ]))
        #expect(summary.replyCount == 6)
        #expect(summary.faces.map(\.name) == ["carol", "dave2", "frank"])
        #expect(summary.faces[1].jid == dave)
        #expect(summary.faces[1].colorKey == dave.description)
        #expect(summary.faces[0].colorKey == "carol")
        #expect(summary.otherParticipantCount == 1)
    }

    @Test func snippetsDescribeRemovedAndAttachmentOnlyReplies() throws {
        let image = SharedFile(url: try #require(URL(string: "https://upload.waddle.test/a/cat.png")), mediaType: "image/png")
        let pdf = SharedFile(url: try #require(URL(string: "https://upload.waddle.test/b/notes.pdf")), mediaType: "application/pdf")
        #expect(ThreadSummary.snippet(of: reply("gone", from: "bob", id: "r1", at: 1, tombstone: .retracted)) == .deleted)
        #expect(ThreadSummary.snippet(of: reply("spam", from: "bob", id: "r2", at: 1, tombstone: .moderated(by: nil, reason: "spam"))) == .removed)
        #expect(ThreadSummary.snippet(of: reply(image.url.absoluteString, from: "bob", id: "r3", at: 1, files: [image])) == .photo)
        #expect(ThreadSummary.snippet(of: reply("", from: "bob", id: "r4", at: 1, files: [pdf])) == .attachment)
        #expect(ThreadSummary.snippet(of: reply("https://media.giphy.com/media/abc/giphy.gif", from: "bob", id: "r5", at: 1)) == .photo)
        #expect(ThreadSummary.snippet(of: reply("look at this", from: "bob", id: "r6", at: 1, files: [pdf])) == .text("look at this"))
    }

    @Test func meActionsNameTheirAuthor() {
        #expect(ThreadSummary.snippet(of: reply("/me waves\nhello", from: "bob", id: "r1", at: 1)) == .action("* bob waves hello"))
    }

    @Test func longSnippetsAreCapped() {
        let long = String(repeating: "word ", count: 100)
        guard case let .text(text) = ThreadSummary.snippet(of: reply(long, from: "bob", id: "r1", at: 1)) else {
            Issue.record("expected text")
            return
        }
        #expect(text.count == ThreadSummary.maxSnippetLength)
    }

    @Test func timelineSummarisesTheThreadUnderARoot() throws {
        let store = TimelineStore()
        store.account = me
        store.ingest(roomMessage("root", from: "bob", stanzaID: "root", at: date(0), source: .archive(mamID: "root")))
        var first = roomMessage("first", from: "carol", stanzaID: "r1", at: date(1), source: .archive(mamID: "r1"))
        first.thread = "root"
        var second = roomMessage("second", from: "dave", stanzaID: "r2", at: date(2), source: .archive(mamID: "r2"))
        second.thread = "root"
        store.ingest(second)
        store.ingest(first)
        let timeline = store.timeline(for: roomConversation)
        let root = try #require(timeline.feedItems.first)
        let summary = try #require(timeline.threadSummary(for: root))
        #expect(summary.replyCount == timeline.replyCount(for: root))
        #expect(summary.replyCount == 2)
        #expect(summary.latestReply?.authorName == "dave")
        #expect(summary.latestReply?.snippet == .text("second"))
        #expect(summary.faces.map(\.name) == ["dave", "carol"])
    }

    @Test func aRowWithoutRepliesHasNoSummary() throws {
        let store = TimelineStore()
        store.account = me
        store.ingest(roomMessage("alone", from: "bob", stanzaID: "s1"))
        let timeline = store.timeline(for: roomConversation)
        let row = try #require(timeline.feedItems.first)
        #expect(timeline.threadSummary(for: row) == nil)
    }
}
