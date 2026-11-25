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
        // An echo carries its own origin id, so others could target it, but
        // it cannot be corrected before it is sent, nor pinned: the server
        // has not seen it.
        #expect(MessageActions.available(for: echo, in: context())
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
        #expect(MessageAction.pin.title == "Pin message")
        #expect(MessageAction.unpin.title == "Unpin message")
        #expect(MessageAction.react("👍").title == "React with 👍")
        #expect(MessageAction.react("👍").accessibilityLabel == "React to message with 👍")
        #expect(MessageAction.reply.accessibilityLabel == "Reply")
    }

    // MARK: Pinning

    @Test func ownersAndAdminsMayPinMembersMightAndInADirectMessageEitherPersonMay() {
        let coordinator = SessionCoordinator(account: me, port: FakePort())
        #expect(coordinator.pinAccess(in: bobConversation) == .allowed)
        // Not in the room: web offers no Pin there either.
        #expect(coordinator.pinAccess(in: roomConversation) == .denied)
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

    private func joined(
        as affiliation: RoomAffiliation, where permission: PinPermission?
    ) -> SessionCoordinator {
        let coordinator = SessionCoordinator(account: me, port: FakePort())
        coordinator.directory.apply(Topology(spaces: [], channels: [
            Channel(roomJID: room, name: "general", pinPermission: permission),
        ]))
        _ = coordinator.presence.apply(WirePresence(
            from: room.with(resource: "alice")!, kind: .available,
            occupant: .init(affiliation: affiliation, role: .participant, realJID: jid("alice@waddle.test"), statusCodes: [110])))
        return coordinator
    }

    /// #2606: the room's disco#info says who may pin, so a member knows
    /// what web's `currentUserCanPin` knows: anyone in the room where
    /// anyone may pin, only owners and admins where only they may.
    @Test(arguments: [
        (PinPermission.anyone, RoomAffiliation.member, PinAccess.allowed),
        (.anyone, .none, .allowed),
        (.adminsOnly, .member, .denied),
        (.adminsOnly, .owner, .allowed),
        (.adminsOnly, .admin, .allowed),
    ])
    func theRoomsPinPermissionDecidesWhetherAMemberMayPin(
        permission: PinPermission, affiliation: RoomAffiliation, expected: PinAccess
    ) {
        #expect(joined(as: affiliation, where: permission).pinAccess(in: roomConversation) == expected)
    }

    private func joinedOnline(
        as affiliation: RoomAffiliation, where permission: PinPermission?
    ) -> (SessionCoordinator, FakePort) {
        let port = FakePort()
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.status.connection = .online
        coordinator.directory.apply(Topology(spaces: [], channels: [
            Channel(roomJID: room, name: "general", pinPermission: permission),
        ]))
        _ = coordinator.presence.apply(WirePresence(
            from: room.with(resource: "alice")!, kind: .available,
            occupant: .init(affiliation: affiliation, role: .participant, realJID: jid("alice@waddle.test"), statusCodes: [110])))
        return (coordinator, port)
    }

    /// The room says its configuration changed (XEP-0045 §10.2.1, status
    /// 104), as when an owner lets anyone pin from another client. Who may
    /// pin is read again from the room, without waiting for a reconnect.
    @Test func aConfigurationChangeReadsWhoMayPinAgain() async {
        let (coordinator, port) = joinedOnline(as: .member, where: .adminsOnly)
        #expect(coordinator.pinAccess(in: roomConversation) == .denied)
        port.roomPinPermissionResult = .success(.anyone)
        coordinator.handle(.roomConfigurationChanged(room))
        await eventually { coordinator.pinAccess(in: roomConversation) == .allowed }
        #expect(coordinator.pinAccess(in: roomConversation) == .allowed)
        #expect(port.roomPinPermissionRequests == [room])
    }

    /// If the room cannot be read, what was known may be out of date: Pin
    /// is offered from the menu, where the server has the last word.
    @Test func aConfigurationChangeThatCannotBeReadLeavesPinsUnknown() async {
        let (coordinator, port) = joinedOnline(as: .member, where: .adminsOnly)
        port.roomPinPermissionResult = .failure(.timeout)
        coordinator.handle(.roomConfigurationChanged(room))
        await eventually { coordinator.pinAccess(in: roomConversation) == .unknown }
        #expect(coordinator.pinAccess(in: roomConversation) == .unknown)
    }

    @Test func aConfigurationChangeFromARoomNotListedReadsNothing() async {
        let (coordinator, port) = joinedOnline(as: .member, where: .adminsOnly)
        coordinator.handle(.roomConfigurationChanged(bare("other@muc.waddle.test")))
        await Task.yield()
        #expect(port.roomPinPermissionRequests.isEmpty)
    }

    @Test func aMemberWhoseRoomSaysNothingOfPinsMightPin() {
        #expect(joined(as: .member, where: nil).pinAccess(in: roomConversation) == .unknown)
    }

    @Test func noOneMayPinInARoomTheyAreNotIn() {
        let coordinator = SessionCoordinator(account: me, port: FakePort())
        coordinator.directory.apply(Topology(spaces: [], channels: [
            Channel(roomJID: room, name: "general", pinPermission: .anyone),
        ]))
        #expect(coordinator.pinAccess(in: roomConversation) == .denied)
    }

    @Test func aMemberSeesPinOnTheToolbarWhereAnyoneMayPin() throws {
        let item = try #require(timeline().item(withID: "root"))
        let pinning = joined(as: .member, where: .anyone).pinAccess(in: roomConversation)
        #expect(MessageActions.toolbar(for: item, in: context(pinning: pinning), isBeingEdited: false, maxButtons: 20)
            .contains(.pin))
        let strict = joined(as: .member, where: .adminsOnly).pinAccess(in: roomConversation)
        #expect(!MessageActions.available(for: item, in: context(pinning: strict)).contains(.pin))
    }
}
