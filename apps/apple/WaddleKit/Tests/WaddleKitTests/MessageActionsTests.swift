import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

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

private let reactions = MessageActions.quickReactions.map(MessageAction.react)

/// One place decides which actions a message row offers, for the hover
/// toolbar, the row's menu and VoiceOver.
@MainActor
@Suite("Message actions")
struct MessageActionsTests {
    private func context(
        thread: String? = nil,
        replyCount: Int = 0,
        isPinned: Bool = false,
        pinning: PinAccess = .allowed
    ) -> MessageActionContext {
        MessageActionContext(thread: thread, replyCount: replyCount, isPinned: isPinned, pinning: pinning)
    }

    @Test func webOffersFiveQuickReactions() {
        #expect(MessageActions.quickReactions == ["👍", "❤️", "😂", "🎉", "👀"])
    }

    @Test func anotherPersonsMessageInTheFeed() throws {
        let item = try #require(timeline().item(withID: "root"))
        #expect(MessageActions.available(for: item, in: context())
            == reactions + [.addReaction, .reply, .startThread, .pin, .selectText])
    }

    @Test func aMessageWithRepliesOpensItsThreadAndAPinnedOneUnpins() throws {
        let item = try #require(timeline().item(withID: "root"))
        #expect(MessageActions.available(for: item, in: context(replyCount: 1, isPinned: true))
            == reactions + [.addReaction, .reply, .openThread, .unpin, .selectText])
    }

    @Test func myOwnMessageAddsEditAndDelete() throws {
        let item = try #require(timeline().item(withID: "m1"))
        #expect(item.isMine)
        #expect(MessageActions.available(for: item, in: context())
            == reactions + [.addReaction, .reply, .startThread, .pin, .selectText, .edit, .delete])
    }

    @Test func insideAThreadAReplyStartsOrOpensItsSubThread() throws {
        let timeline = timeline()
        let reply = try #require(timeline.item(withID: "r1"))
        let root = try #require(timeline.item(withID: "root"))
        #expect(MessageActions.available(for: reply, in: context(thread: "root")).contains(.startThread))
        #expect(MessageActions.available(for: reply, in: context(thread: "root", replyCount: 2)).contains(.openThread))
        let rootActions = MessageActions.available(for: root, in: context(thread: "root"))
        #expect(!rootActions.contains(.startThread) && !rootActions.contains(.openThread))
    }

    @Test func pinFollowsWhatTheRoomAllows() throws {
        let item = try #require(timeline().item(withID: "root"))
        #expect(!MessageActions.available(for: item, in: context(pinning: .denied)).contains(.pin))
        // A member may pin only where the room lets anyone pin, which the
        // app cannot read: the menu offers it, the toolbar does not.
        #expect(MessageActions.available(for: item, in: context(pinning: .unknown)).contains(.pin))
        #expect(!MessageActions.toolbar(for: item, in: context(pinning: .unknown), isBeingEdited: false, maxButtons: 20).contains(.pin))
        #expect(MessageActions.toolbar(for: item, in: context(pinning: .allowed), isBeingEdited: false, maxButtons: 20).contains(.pin))
    }

    @Test func selectTextNeedsWords() throws {
        let store = TimelineStore()
        store.account = me
        var image = roomMessage("https://media.giphy.com/media/abc/giphy.gif", from: "bob", stanzaID: "g1")
        image.thread = nil
        store.ingest(image)
        let item = try #require(store.timeline(for: roomConversation).item(withID: "g1"))
        #expect(!MessageActions.available(for: item, in: context()).contains(.selectText))
    }

    @Test func aDeletedMessageHasNoActions() throws {
        var item = try #require(timeline().item(withID: "r1"))
        item.tombstone = .retracted
        #expect(MessageActions.available(for: item, in: context(thread: "root", replyCount: 2)).isEmpty)
        #expect(MessageActions.toolbar(for: item, in: context(thread: "root"), isBeingEdited: false, maxButtons: 20).isEmpty)
    }

    @Test func anUnsentEchoCannotBeEditedYet() async throws {
        let coordinator = SessionCoordinator(account: me, port: FakePort())
        coordinator.status.connection = .offline(retryAt: nil)
        let id = try #require(await coordinator.send(Draft(text: "queued"), in: bobConversation))
        let echo = try #require(coordinator.timelines.timeline(for: bobConversation).item(withID: id))
        // 1:1 has no pins; an echo carries its own origin id, so others
        // could target it, but it cannot be corrected before it is sent.
        #expect(MessageActions.available(for: echo, in: context(pinning: .denied))
            == reactions + [.addReaction, .reply, .startThread, .selectText, .delete])
    }

    // MARK: Toolbar

    @Test func theToolbarLeavesOutSelectText() throws {
        let item = try #require(timeline().item(withID: "m1"))
        #expect(MessageActions.toolbar(for: item, in: context(), isBeingEdited: false, maxButtons: 20)
            == reactions + [.addReaction, .reply, .startThread, .pin, .edit, .delete])
    }

    @Test func aNarrowRowDropsQuickReactionsFirst() throws {
        let item = try #require(timeline().item(withID: "m1"))
        #expect(MessageActions.toolbar(for: item, in: context(), isBeingEdited: false, maxButtons: 8)
            == [.react("👍"), .react("❤️"), .addReaction, .reply, .startThread, .pin, .edit, .delete])
        // Never below what is not a quick reaction.
        #expect(MessageActions.toolbar(for: item, in: context(), isBeingEdited: false, maxButtons: 2)
            == [.addReaction, .reply, .startThread, .pin, .edit, .delete])
    }

    @Test func aMessageBeingEditedHasNoToolbar() throws {
        let item = try #require(timeline().item(withID: "m1"))
        #expect(MessageActions.toolbar(for: item, in: context(), isBeingEdited: true, maxButtons: 20).isEmpty)
    }

    @Test func labelsAreTheSameEverywhere() {
        #expect(MessageAction.startThread.title == "Reply in thread")
        #expect(MessageAction.openThread.title == "Open thread")
        #expect(MessageAction.pin.title == "Pin to room")
        #expect(MessageAction.unpin.title == "Unpin from room")
        #expect(MessageAction.react("👍").title == "React with 👍")
        #expect(MessageAction.react("👍").accessibilityLabel == "React to message with 👍")
        #expect(MessageAction.reply.accessibilityLabel == "Reply")
    }

    // MARK: Pinning

    @Test func ownersAndAdminsMayPinMembersMightDirectMessagesCannot() {
        let coordinator = SessionCoordinator(account: me, port: FakePort())
        #expect(coordinator.pinAccess(in: bobConversation) == .denied)
        #expect(coordinator.pinAccess(in: roomConversation) == .unknown)
        for (affiliation, expected) in [(RoomAffiliation.owner, PinAccess.allowed), (.admin, .allowed), (.member, .unknown)] {
            _ = coordinator.presence.apply(WirePresence(
                from: room.with(resource: "alice")!,
                kind: .available,
                show: nil,
                status: nil,
                hats: [],
                occupant: .init(affiliation: affiliation, role: .participant, realJID: jid("alice@waddle.test"), statusCodes: [110]),
                idleSince: nil
            ))
            #expect(coordinator.pinAccess(in: roomConversation) == expected)
        }
    }
}
