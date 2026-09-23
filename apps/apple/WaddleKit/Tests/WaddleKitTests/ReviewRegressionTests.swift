import Foundation
import Testing
@testable import WaddleKit

/// Regressions for the adversarial review of the core.
@MainActor
@Suite("Review regressions")
struct ReviewRegressionTests {
    private func online() -> (SessionCoordinator, FakePort) {
        let port = FakePort()
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.status.connection = .online
        coordinator.isSendReady = true
        coordinator.directory.apply(Topology(spaces: [], channels: [Channel(roomJID: room, name: "general")]))
        return (coordinator, port)
    }

    @Test func fakePortProbeUsesScriptedResult() async {
        let port = FakePort()
        #expect(await port.probeConnection())

        port.probeConnectionResult = false
        #expect(await !port.probeConnection())
        #expect(port.probeConnectionCount == 2)
    }

    /// An occupant injecting a foreign stanza-id equal to another row's
    /// room id must not make that row unmoderatable.
    @Test func injectedForeignStanzaIDDoesNotBlockModeration() {
        let store = TimelineStore()
        store.account = me
        store.ingest(roomMessage("spam", from: "eve", stanzaID: "s1"))
        var injected = roomMessage("decoy", from: "eve2", stanzaID: "s2")
        injected.identity = MessageIdentity(
            messageID: "m2",
            stanzaID: StanzaID(id: "s1", by: bare("evil.example")),
            stanzaIDs: [StanzaID(id: "s1", by: bare("evil.example")), StanzaID(id: "s2", by: room)]
        )
        store.ingest(injected)
        var moderation = WireMessage(type: .groupchat, from: JID(bare: room, resource: nil), to: nil, identity: MessageIdentity())
        moderation.moderation = .init(targetID: "s1", moderatedBy: nil, reason: nil)
        store.ingest(moderation)
        let rows = store.timeline(for: roomConversation).items
        #expect(rows.first { $0.body == "spam" }?.tombstone != nil)
        #expect(rows.first { $0.body == "decoy" }?.tombstone == nil)
    }

    /// An occupant reusing our client id as their @id cannot hijack
    /// reactions aimed at our message.
    @Test func reusedClientIDDoesNotStealReactions() {
        let store = TimelineStore()
        store.account = me
        store.ingest(roomMessage("mine", from: "alice", stanzaID: "room-1", originID: "c1"))
        store.ingest(roomMessage("copycat", from: "eve", stanzaID: "room-2", originID: "room-1"))
        store.ingest(reaction(["👍"], to: "room-1", from: "bob"))
        let rows = store.timeline(for: roomConversation).items
        #expect(rows.first { $0.body == "mine" }?.reactions.map(\.emoji) == ["👍"])
        #expect(rows.first { $0.body == "copycat" }?.reactions.isEmpty == true)
    }

    @Test func roomRetractionWaitsForReflection() async {
        let (coordinator, _) = online()
        coordinator.route(roomMessage("oops", from: "alice", stanzaID: "s1", originID: "c1"))
        let item = coordinator.timelines.timeline(for: roomConversation).items[0]
        #expect(await coordinator.retract(item))
        #expect(coordinator.timelines.timeline(for: roomConversation).items[0].tombstone == nil)
        var reflected = roomMessage(nil, from: "alice", stanzaID: "s2")
        reflected.retractsID = "s1"
        coordinator.route(reflected)
        #expect(coordinator.timelines.timeline(for: roomConversation).items[0].tombstone == .retracted)
    }

    @Test func queueKeepsOrderAcrossTransientFailure() async {
        let port = FakePort()
        var failOnce = true
        port.sendOutcome = { message in
            if failOnce {
                failOnce = false
                return .notConnected
            }
            return .sent(stanzaID: message.clientID)
        }
        let coordinator = SessionCoordinator(
            account: me,
            port: port,
            reconnectPolicy: ReconnectPolicy(base: 0.01, cap: 0.01)
        )
        let first = await coordinator.send(Draft(text: "1"), in: bobConversation)!
        let second = await coordinator.send(Draft(text: "2"), in: bobConversation)!

        coordinator.start()
        port.emit(.connected)
        await eventually { port.disconnectCount == 1 }
        #expect(coordinator.outboundQueue.map(\.clientID) == [first, second])
        await eventually { port.connectCount >= 2 }
        port.emit(.connected)
        await eventually { coordinator.isSendReady && coordinator.outboundQueue.isEmpty }
        #expect(port.sent.map(\.clientID) == [first, first, second])
        // A send while not ready never overtakes the queue.
        coordinator.isSendReady = false
        let third = await coordinator.send(Draft(text: "3"), in: bobConversation)!
        #expect(port.sent.last?.clientID == second)
        #expect(coordinator.outboundQueue.map(\.clientID) == [third])
        await coordinator.stop()
    }

    @Test func deliveryFailureAfterWriteIsRetryable() async {
        let (coordinator, port) = online()
        let id = await coordinator.send(Draft(text: "hi"), in: bobConversation)!
        coordinator.handle(.deliveryFailed(stanzaID: id))
        #expect(coordinator.deliveries.state(of: id) == .failed)
        await coordinator.retry(clientID: id)
        #expect(port.sent.map(\.clientID) == [id, id])
        #expect(coordinator.deliveries.state(of: id) == .sent)
    }

    @Test func errorBounceMarksOwnSendFailed() async {
        let (coordinator, _) = online()
        let id = await coordinator.send(Draft(text: "hi"), in: roomConversation)!
        coordinator.handle(.deliveryAcked(stanzaID: id))
        var bounce = WireMessage(type: .error, from: JID(bare: room, resource: nil), to: nil, identity: MessageIdentity(messageID: id))
        bounce.body = "hi"
        coordinator.route(bounce)
        #expect(coordinator.deliveries.state(of: id) == .failed)
    }

    @Test func leavingBeforeHistoryLoadsSendsNoMarker() async {
        let (coordinator, port) = online()
        coordinator.route(roomMessage("unseen", from: "bob", stanzaID: "s1"))
        coordinator.visibleConversation = nil
        await coordinator.markDisplayedIfVisible(roomConversation)
        #expect(port.displayed.isEmpty)
        #expect(coordinator.unread.count(for: roomConversation) == 1)
    }

    @Test func cursorAfterGapKeepsInboxCount() {
        let (coordinator, _) = online()
        coordinator.route(roomMessage("old", from: "bob", stanzaID: "s1"))
        coordinator.handle(.disconnected)
        coordinator.handle(.inboxPush(InboxEntry(partner: room, kind: .room, lastStanzaID: "s30", lastUpdated: 30, unread: 20, preview: nil, threadID: nil)))
        coordinator.applyDisplayedCursor(DisplayedCursor(conversation: room, stanzaID: "s1", stanzaIDBy: room))
        #expect(coordinator.unread.count(for: roomConversation) == 20)
    }

    @Test func directMarkerCopiesMessageID() async {
        let (coordinator, port) = online()
        var message = directMessage("hi", from: jid("bob@waddle.test/l"), to: jid("alice@waddle.test/p"), id: "stanza-id-attr")
        message.identity = MessageIdentity(messageID: "stanza-id-attr", originID: "origin-x")
        message.displayedMarkerRequested = true
        coordinator.route(message)
        await coordinator.markDisplayed(bobConversation)
        #expect(port.displayed.map(\.id) == ["stanza-id-attr"])
    }

    @Test func correctionReplacesMarkupWithTheText() {
        let store = TimelineStore()
        store.account = me
        var original = roomMessage("hi there", from: "bob", stanzaID: "s1", originID: "o1")
        original.markupSpans = [MarkupSpan(kind: .bold, start: 0, end: 2)]
        store.ingest(original)
        var correction = roomMessage("hello there", from: "bob", stanzaID: "s2")
        correction.replacesID = "o1"
        correction.markupSpans = [MarkupSpan(kind: .italic, start: 6, end: 11)]
        store.ingest(correction)
        let row = store.timeline(for: roomConversation).items[0]
        #expect(row.body == "hello there")
        #expect(row.message.markupSpans == [MarkupSpan(kind: .italic, start: 6, end: 11)])
    }

    @Test func editResendsReplyAndAttachments() async {
        let (coordinator, port) = online()
        let file = SharedFile(url: URL(string: "https://files.waddle.test/a.png")!, mediaType: "image/png", disposition: .inline)
        let reply = ReplyContext(targetID: "p1", author: jid("bob@waddle.test"), parentBody: "q", parentAuthorName: "bob")
        await coordinator.send(Draft(text: "caption", reply: reply, attachments: [file]), in: bobConversation)
        let item = coordinator.timelines.timeline(for: bobConversation).items[0]
        #expect(await coordinator.edit(item, draft: Draft(text: "better caption")))
        #expect(port.corrections.last?.body.hasSuffix("better caption") == true)
        let row = coordinator.timelines.timeline(for: bobConversation).items[0]
        #expect(row.body == "better caption")
        #expect(row.message.sharedFiles == [file])
    }
}
