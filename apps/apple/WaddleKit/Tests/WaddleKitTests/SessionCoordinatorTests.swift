import Foundation
import Testing
@testable import WaddleKit

@MainActor
@Suite("Session coordinator")
struct SessionCoordinatorTests {
    private func online(_ port: FakePort = FakePort()) -> (SessionCoordinator, FakePort) {
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.status.connection = .online
        coordinator.directory.apply(Topology(spaces: [], channels: [Channel(roomJID: room, name: "general")]))
        return (coordinator, port)
    }

    @Test func sendShowsLocalEchoAndTracksDelivery() async {
        let (coordinator, port) = online()
        let id = await coordinator.send(Draft(text: "  hello **world**  "), in: roomConversation)
        #expect(port.sent.count == 1)
        #expect(port.sent[0].body == "hello world")
        #expect(port.sent[0].options.markupSpans == [MarkupSpan(kind: .bold, start: 6, end: 11)])
        #expect(coordinator.deliveries.state(of: id!) == .sent)
        let row = coordinator.timelines.timeline(for: roomConversation).items[0]
        #expect(row.isLocalEcho)
        #expect(row.body == "hello world")

        coordinator.handle(.deliveryAcked(stanzaID: id!))
        #expect(coordinator.deliveries.state(of: id!) == .acknowledged)
    }

    @Test func offlineSendQueuesAndFlushesOnReady() async {
        let (coordinator, port) = online()
        coordinator.status.connection = .offline(retryAt: nil)
        let id = await coordinator.send(Draft(text: "later"), in: bobConversation)
        #expect(port.sent.isEmpty)
        #expect(coordinator.deliveries.state(of: id!) == .queued)

        coordinator.status.connection = .online
        await coordinator.flushOutboundQueue()
        #expect(port.sent.map(\.clientID) == [id!])
        #expect(coordinator.deliveries.state(of: id!) == .sent)
    }

    @Test func rejectedSendCanBeRetried() async {
        let port = FakePort()
        port.sendOutcome = { _ in .rejected }
        let (coordinator, _) = online(port)
        let id = await coordinator.send(Draft(text: "nope"), in: roomConversation)!
        #expect(coordinator.deliveries.state(of: id) == .failed)
        port.sendOutcome = { .sent(stanzaID: $0.clientID) }
        await coordinator.retry(clientID: id)
        #expect(coordinator.deliveries.state(of: id) == .sent)
        #expect(port.sent.map(\.clientID) == [id, id])
    }

    @Test func replyCarriesFallbackAndShiftedOffsets() async {
        let (coordinator, port) = online()
        let reply = ReplyContext(targetID: "s1", author: jid("general@muc.waddle.test/bob"), parentBody: "hi", parentAuthorName: "bob")
        await coordinator.send(Draft(text: "*yo*", reply: reply), in: roomConversation)
        let sent = port.sent[0]
        #expect(sent.body == "> hi\n\nyo")
        #expect(sent.options.reply?.fallback == 0..<6)
        #expect(sent.options.markupSpans == [MarkupSpan(kind: .italic, start: 6, end: 8)])
    }

    @Test func incomingMessageCountsUnreadAndAlertsOnMention() async {
        let (coordinator, _) = online()
        var alerts: [IncomingAlert] = []
        coordinator.onAlert = { alerts.append($0) }
        coordinator.route(roomMessage("plain", from: "bob", stanzaID: "s1"))
        var mention = roomMessage("@alice look", from: "bob", stanzaID: "s2")
        mention.references = [Reference(kind: .mention, uri: "xmpp:alice@waddle.test", begin: 0, end: 6)]
        coordinator.route(mention)
        #expect(coordinator.unread.count(for: roomConversation) == 2)
        #expect(coordinator.unread.mentions.contains(roomConversation))
        // Rooms default to on-mention alerts.
        #expect(alerts.map(\.body) == ["@alice look"])
    }

    @Test func directMessagesAlertAndUpdateRecency() {
        let (coordinator, _) = online()
        var alerts: [IncomingAlert] = []
        coordinator.onAlert = { alerts.append($0) }
        coordinator.route(directMessage("yo", from: jid("bob@waddle.test/laptop"), to: jid("alice@waddle.test/phone"), id: "d1"))
        #expect(alerts.count == 1)
        #expect(coordinator.directory.directConversations.first?.peer == bob)
        #expect(coordinator.unread.count(for: bobConversation) == 1)
    }

    @Test func mutationsDoNotAlertOrCount() {
        let (coordinator, _) = online()
        coordinator.route(roomMessage("msg", from: "bob", stanzaID: "s1"))
        coordinator.unread.clear(roomConversation)
        coordinator.route(reaction(["👍"], to: "s1", from: "carol"))
        #expect(coordinator.unread.count(for: roomConversation) == 0)
    }

    @Test func openingMarksDisplayedWithRoomStanzaID() async {
        let (coordinator, port) = online()
        coordinator.route(roomMessage("msg", from: "bob", stanzaID: "s1"))
        await coordinator.open(roomConversation)
        #expect(port.displayed.map(\.id) == ["s1"])
        #expect(port.publishedCursors == [DisplayedCursor(conversation: room, stanzaID: "s1", stanzaIDBy: room)])
        #expect(coordinator.unread.count(for: roomConversation) == 0)
        // Deduped: nothing new to mark.
        await coordinator.markDisplayed(roomConversation)
        #expect(port.displayed.count == 1)
    }

    @Test func directMarkerOnlyWhenRequested() async {
        let (coordinator, port) = online()
        coordinator.route(directMessage("no request", from: jid("bob@waddle.test/l"), to: jid("alice@waddle.test/p"), id: "d1", archiveID: "a1"))
        await coordinator.markDisplayed(bobConversation)
        #expect(port.displayed.isEmpty)
        #expect(port.publishedCursors == [DisplayedCursor(conversation: bob, stanzaID: "a1", stanzaIDBy: me.jid)])

        var requested = directMessage("please", from: jid("bob@waddle.test/l"), to: jid("alice@waddle.test/p"), id: "d2", archiveID: "a2")
        requested.displayedMarkerRequested = true
        coordinator.route(requested)
        await coordinator.markDisplayed(bobConversation)
        #expect(port.displayed.map(\.id) == ["d2"])
    }

    @Test func offlineReadIsParkedAndReplayed() async {
        let (coordinator, port) = online()
        coordinator.route(roomMessage("msg", from: "bob", stanzaID: "s1"))
        coordinator.status.connection = .offline(retryAt: nil)
        await coordinator.markDisplayed(roomConversation)
        #expect(port.displayed.isEmpty)
        coordinator.status.connection = .online
        await coordinator.drainPendingDisplayed()
        #expect(port.displayed.map(\.id) == ["s1"])
    }

    @Test func siblingCursorRecomputesUnread() {
        let (coordinator, _) = online()
        coordinator.route(roomMessage("1", from: "bob", stanzaID: "s1"))
        coordinator.route(roomMessage("2", from: "bob", stanzaID: "s2"))
        coordinator.route(roomMessage("3", from: "bob", stanzaID: "s3"))
        #expect(coordinator.unread.count(for: roomConversation) == 3)
        coordinator.applyDisplayedCursor(DisplayedCursor(conversation: room, stanzaID: "s2", stanzaIDBy: room))
        #expect(coordinator.unread.count(for: roomConversation) == 1)
        // An older cursor never regresses.
        coordinator.applyDisplayedCursor(DisplayedCursor(conversation: room, stanzaID: "s1", stanzaIDBy: room))
        #expect(coordinator.unread.count(for: roomConversation) == 1)
    }

    @Test func inboxPushSetsUnreadAndSuppressesDoubleCount() {
        let (coordinator, _) = online()
        coordinator.handle(.inboxPush(InboxEntry(partner: room, kind: .room, lastStanzaID: "s7", lastUpdated: 7, unread: 4, preview: nil, threadID: nil)))
        #expect(coordinator.unread.count(for: roomConversation) == 4)
        coordinator.route(roomMessage("counted already", from: "bob", stanzaID: "s7"))
        #expect(coordinator.unread.count(for: roomConversation) == 4)
    }

    @Test func toggleReactionSendsFullSetAndAppliesLocally() async {
        let (coordinator, port) = online()
        coordinator.route(directMessage("hi", from: jid("bob@waddle.test/l"), to: jid("alice@waddle.test/p"), id: "d1"))
        let item = coordinator.timelines.timeline(for: bobConversation).items[0]
        #expect(await coordinator.toggleReaction("👍", on: item))
        #expect(port.reactions.last?.target == "d1")
        #expect(port.reactions.last?.emojis == ["👍"])
        let reacted = coordinator.timelines.timeline(for: bobConversation).items[0]
        #expect(reacted.reactions.first?.includesMine == true)
        #expect(await coordinator.toggleReaction("👍", on: reacted))
        #expect(port.reactions.last?.emojis == [])
        #expect(coordinator.timelines.timeline(for: bobConversation).items[0].reactions.isEmpty)
    }

    @Test func editAndRetractOwnDirectMessage() async {
        let (coordinator, port) = online()
        await coordinator.send(Draft(text: "tpyo"), in: bobConversation)
        let item = coordinator.timelines.timeline(for: bobConversation).items[0]
        #expect(await coordinator.edit(item, to: "typo"))
        #expect(port.corrections.last?.target == item.id)
        #expect(coordinator.timelines.timeline(for: bobConversation).items[0].body == "typo")
        #expect(await coordinator.retract(item))
        #expect(coordinator.timelines.timeline(for: bobConversation).items[0].tombstone == .retracted)
    }

    @Test func historyPagesWithRSMCursor() async {
        let (coordinator, port) = online()
        port.historyPages = [
            ArchivePage(messages: [roomMessage("new", from: "bob", stanzaID: "s2", at: date(2), source: .archive(mamID: "s2"))], first: "s2", isComplete: false),
            ArchivePage(messages: [roomMessage("old", from: "bob", stanzaID: "s1", at: date(1), source: .archive(mamID: "s1"))], first: "s1", isComplete: true),
        ]
        await coordinator.loadLatest(roomConversation)
        #expect(coordinator.history.state(of: roomConversation).hasMoreOlder)
        await coordinator.loadOlder(roomConversation)
        #expect(port.historyRequests.map(\.1) == [nil, "s2"])
        #expect(!coordinator.history.state(of: roomConversation).hasMoreOlder)
        #expect(coordinator.timelines.timeline(for: roomConversation).items.map(\.body) == ["old", "new"])
        // Archived history never counts as unread.
        #expect(coordinator.unread.count(for: roomConversation) == 0)
    }

    @Test func authenticationFailureStopsReconnecting() {
        let (coordinator, _) = online()
        var signedOut = false
        coordinator.onAuthenticationFailed = { signedOut = true }
        coordinator.handle(.authenticationFailed)
        coordinator.handle(.disconnected)
        #expect(signedOut)
        #expect(coordinator.connection == .authenticationFailed)
    }
}
