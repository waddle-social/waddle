import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

private let reactions = ["👍", "❤️"]

@MainActor
private func timeline() -> ConversationTimeline {
    let store = TimelineStore()
    store.account = me
    store.ingest(roomMessage("root", from: "bob", stanzaID: "root"))
    var reply = roomMessage("reply", from: "carol", stanzaID: "r1")
    reply.thread = "root"
    store.ingest(reply)
    var own = roomMessage("mine", from: "alice", stanzaID: "m1", originID: "m1")
    own.authorRealJID = me.jid
    store.ingest(own)
    return store.timeline(for: roomConversation)
}

/// The hover toolbar on a message row offers what web's does, in its
/// order, under the context menu's conditions.
@MainActor
@Suite("Message toolbar")
struct MessageToolbarTests {
    @Test func anotherPersonsMessageInTheFeed() throws {
        let item = try #require(timeline().item(withID: "root"))
        #expect(MessageToolbar.actions(for: item, inThread: nil, replyCount: 0, isPinned: false, quickReactions: reactions) == [
            .react("👍"), .react("❤️"), .addReaction, .reply, .thread(opens: false), .pin(isPinned: false),
        ])
    }

    @Test func aMessageWithRepliesOpensItsThread() throws {
        let item = try #require(timeline().item(withID: "root"))
        let actions = MessageToolbar.actions(for: item, inThread: nil, replyCount: 1, isPinned: true, quickReactions: [])
        #expect(actions == [.addReaction, .reply, .thread(opens: true), .pin(isPinned: true)])
    }

    @Test func myOwnMessageAddsEditAndDelete() throws {
        let item = try #require(timeline().item(withID: "m1"))
        #expect(item.isMine)
        let actions = MessageToolbar.actions(for: item, inThread: nil, replyCount: 0, isPinned: false, quickReactions: [])
        #expect(actions == [.addReaction, .reply, .thread(opens: false), .pin(isPinned: false), .edit, .delete])
    }

    @Test func insideAThreadAReplyStartsOrOpensItsSubThread() throws {
        let timeline = timeline()
        let reply = try #require(timeline.item(withID: "r1"))
        let root = try #require(timeline.item(withID: "root"))
        #expect(MessageToolbar.actions(for: reply, inThread: "root", replyCount: 0, isPinned: false, quickReactions: [])
            .contains(.thread(opens: false)))
        #expect(MessageToolbar.actions(for: reply, inThread: "root", replyCount: 2, isPinned: false, quickReactions: [])
            .contains(.thread(opens: true)))
        // The open thread's own root has no thread button there.
        #expect(MessageToolbar.actions(for: root, inThread: "root", replyCount: 0, isPinned: false, quickReactions: [])
            == [.addReaction, .reply, .pin(isPinned: false)])
    }

    @Test func aDeletedMessageHasNoToolbar() throws {
        var item = try #require(timeline().item(withID: "r1"))
        item.tombstone = .retracted
        #expect(MessageToolbar.actions(for: item, inThread: "root", replyCount: 2, isPinned: false, quickReactions: reactions).isEmpty)
    }

    @Test func aRowNobodyCanTargetYetOffersOnlyWhatNeedsNoID() async throws {
        let coordinator = SessionCoordinator(account: me, port: FakePort())
        coordinator.status.connection = .online
        coordinator.isSendReady = true
        coordinator.status.connection = .offline(retryAt: nil)
        let id = try #require(await coordinator.send(Draft(text: "queued"), in: bobConversation))
        let echo = try #require(coordinator.timelines.timeline(for: bobConversation).item(withID: id))
        // A 1:1 echo carries its own origin id, so it can be targeted, but
        // an unsent echo cannot be edited yet; 1:1 has no pins.
        #expect(MessageToolbar.actions(for: echo, inThread: nil, replyCount: 0, isPinned: false, quickReactions: [])
            == [.addReaction, .reply, .thread(opens: false), .delete])
    }
}
