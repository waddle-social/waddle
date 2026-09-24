import Foundation
import Testing
@testable import WaddleKit

/// Delivery settling (XEP-0198), sender-less 1:1 routing (RFC 6120) and
/// cursor authority (XEP-0490).
@MainActor
@Suite("Delivery and trust")
struct DeliveryAndTrustTests {
    @Test func ackDuringSendIsNotDowngraded() {
        let store = DeliveryStore()
        store.began("c1")
        store.acknowledged("c1")
        store.outcome(.sent(stanzaID: "c1"), for: "c1")
        #expect(store.state(of: "c1") == .acknowledged)
    }

    @Test func ackedSendIsNotRequeuedAfterTransportError() {
        let store = DeliveryStore()
        store.began("c1")
        store.acknowledged("c1")
        store.outcome(.transportError, for: "c1")
        #expect(store.state(of: "c1") == .acknowledged)
    }

    @Test func failureDuringSendIsKept() {
        let store = DeliveryStore()
        store.began("c1")
        store.failed("c1")
        store.outcome(.sent(stanzaID: "c1"), for: "c1")
        #expect(store.state(of: "c1") == .failed)
    }

    @Test func earlyAckIsConsumedSoARetryStartsClean() {
        let store = DeliveryStore()
        store.acknowledged("c1")
        store.began("c1")
        store.outcome(.sent(stanzaID: "c1"), for: "c1")
        #expect(store.state(of: "c1") == .acknowledged)
        store.forget("c1")
        store.began("c1")
        store.outcome(.sent(stanzaID: "c1"), for: "c1")
        #expect(store.state(of: "c1") == .sent)
    }

    @Test func senderlessDirectStanzaIsNotRouted() {
        #expect(me.route(from: nil, to: JID(bare: me.jid, resource: "phone"), isGroupchat: false) == nil)
    }

    @Test func ownCarbonStillRoutesToThePeer() {
        let bob = bare("bob@waddle.test")
        let route = me.route(from: JID(bare: me.jid, resource: "laptop"), to: JID(bare: bob, resource: nil), isGroupchat: false)
        #expect(route == MessageRoute(conversation: .direct(bob), isMine: true))
    }

    @Test func roomCursorFromAnotherAuthorityIsIgnored() {
        let port = FakePort()
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.directory.apply(Topology(spaces: [], channels: [Channel(roomJID: room, name: "general")]))
        coordinator.route(roomMessage("one", from: "bob", stanzaID: "s1"))
        // An occupant can put a stanza-id from any authority on their message.
        var injected = roomMessage("two", from: "eve", stanzaID: "s2")
        let foreign = StanzaID(id: "x9", by: bare("evil.example"))
        injected.identity = MessageIdentity(
            stanzaID: StanzaID(id: "s2", by: room),
            stanzaIDs: [StanzaID(id: "s2", by: room), foreign]
        )
        coordinator.route(injected)
        coordinator.applyDisplayedCursor(DisplayedCursor(conversation: room, stanzaID: "x9", stanzaIDBy: bare("evil.example")))
        #expect(coordinator.readCursors.cursor(roomConversation) == nil)
        coordinator.applyDisplayedCursor(DisplayedCursor(conversation: room, stanzaID: "s2", stanzaIDBy: room))
        #expect(coordinator.readCursors.cursor(roomConversation) == "s2")
    }
}

@MainActor
@Suite("Delivery and trust, round two")
struct DeliveryAndTrustRoundTwoTests {
    @Test func failureDuringSendSurvivesTransportError() {
        let store = DeliveryStore()
        store.began("c1")
        store.failed("c1")
        store.outcome(.transportError, for: "c1")
        #expect(store.state(of: "c1") == .failed)
    }

    @Test func failedMidSendLeavesTheQueueForRetry() async {
        let port = FakePort()
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.status.connection = .online
        coordinator.isSendReady = true
        let bob = bare("bob@waddle.test")
        port.sendOutcome = { [weak coordinator] message in
            coordinator?.deliveries.failed(message.clientID)
            return .transportError
        }
        await coordinator.send(Draft(text: "hi"), in: .direct(bob))
        let id = port.sent.first?.clientID
        #expect(id != nil)
        #expect(coordinator.outboundQueue.isEmpty)
        #expect(coordinator.failedOutbound[id ?? ""] != nil)
        #expect(coordinator.deliveries.state(of: id ?? "") == .failed)
    }

    /// A 1:1 cursor our own devices publish with the domain as authority.
    @Test func directCursorWithDomainAuthorityIsApplied() {
        let port = FakePort()
        let coordinator = SessionCoordinator(account: me, port: port)
        let bob = bare("bob@waddle.test")
        let domain = BareJID(localpart: nil, domain: me.jid.domain)!
        var message = directMessage("hi", from: jid("bob@waddle.test/a"), to: jid("alice@waddle.test/phone"), id: "m1")
        message.identity = MessageIdentity(messageID: "m1", stanzaID: StanzaID(id: "a1", by: domain), stanzaIDs: [StanzaID(id: "a1", by: domain)])
        coordinator.route(message)
        coordinator.applyDisplayedCursor(DisplayedCursor(conversation: bob, stanzaID: "a1", stanzaIDBy: domain))
        #expect(coordinator.readCursors.cursor(.direct(bob)) == "a1")
    }
}

@MainActor
@Suite("Typed message rejection")
struct MessageRejectionTests {
    private func online() -> (SessionCoordinator, FakePort, InMemoryOutboxStore) {
        let port = FakePort()
        let store = InMemoryOutboxStore()
        let coordinator = SessionCoordinator(account: me, port: port, outboxStore: store)
        coordinator.restoreOutboxIfNeeded()
        coordinator.status.connection = .online
        coordinator.isSendReady = true
        return (coordinator, port, store)
    }

    @Test(arguments: [false, true])
    func rejectionBeforeOrAfterAckStaysFailedAndDoesNotRetry(ackFirst: Bool) async throws {
        let (coordinator, port, store) = online()
        let id = try #require(await coordinator.send(Draft(text: "hi"), in: bobConversation))
        if ackFirst { coordinator.handle(.deliveryAcked(stanzaID: id)) }
        coordinator.handle(.messageRejected(stanzaID: id, from: jid("bob@waddle.test/phone"), to: jid("alice@waddle.test/mac")))
        coordinator.handle(.deliveryAcked(stanzaID: id))
        #expect(coordinator.deliveries.state(of: id) == .failed)
        #expect(coordinator.outboundQueue.isEmpty)
        #expect(coordinator.sentOutbound.isEmpty)
        #expect(coordinator.failedOutbound[id] != nil)
        #expect(store.entries.map(\.state) == [.failed])
        #expect(coordinator.timelines.timeline(for: bobConversation).items.count == 1)
        #expect(coordinator.unread.total == 0)
        coordinator.requeueUnconfirmedSendsForFreshStream()
        await coordinator.flushOutboundQueue()
        #expect(port.sent.count == 1)

        let restoredPort = FakePort()
        let restored = SessionCoordinator(account: me, port: restoredPort, outboxStore: store)
        restored.restoreOutboxIfNeeded()
        restored.status.connection = .online
        restored.isSendReady = true
        await restored.flushOutboundQueue()
        #expect(restored.deliveries.state(of: id) == .failed)
        #expect(restoredPort.sent.isEmpty)
    }

    @Test func rejectionWhileSendIsSuspendedWinsOverItsOutcome() async throws {
        let (coordinator, port, _) = online()
        port.sendOutcome = { [weak coordinator] message in
            coordinator?.handle(.messageRejected(stanzaID: message.clientID, from: jid("bob@waddle.test"), to: nil))
            coordinator?.handle(.deliveryAcked(stanzaID: message.clientID))
            return .sent(stanzaID: message.clientID)
        }
        let id = try #require(await coordinator.send(Draft(text: "hi"), in: bobConversation))
        #expect(coordinator.deliveries.state(of: id) == .failed)
        #expect(coordinator.outboundQueue.isEmpty)
        #expect(coordinator.sentOutbound.isEmpty)
        #expect(coordinator.failedOutbound[id] != nil)
    }

    @Test func delayedRejectionSurvivesMoreThan200OtherAcknowledgedSends() async throws {
        let (coordinator, port, store) = online()
        let id = try #require(await coordinator.send(Draft(text: "first"), in: bobConversation))
        coordinator.handle(.deliveryAcked(stanzaID: id))
        for _ in 0..<250 {
            let other = try #require(await coordinator.send(Draft(text: "later"), in: roomConversation))
            coordinator.handle(.deliveryAcked(stanzaID: other))
        }
        coordinator.handle(.messageRejected(stanzaID: id, from: jid("eve@waddle.test"), to: nil))
        #expect(coordinator.deliveries.state(of: id) == .acknowledged)
        coordinator.handle(.messageRejected(stanzaID: id, from: jid("bob@waddle.test"), to: nil))
        coordinator.handle(.deliveryAcked(stanzaID: id))
        #expect(coordinator.deliveries.state(of: id) == .failed)
        #expect(coordinator.timelines.timeline(for: bobConversation).items.count == 1)
        #expect(store.entries.map(\.state) == [.failed])
        coordinator.requeueUnconfirmedSendsForFreshStream()
        await coordinator.flushOutboundQueue()
        #expect(port.sent.count == 251)
        await coordinator.stop()
        #expect(coordinator.recentlyAcknowledgedOutbound.isEmpty)
    }

    @Test(arguments: ["eve@waddle.test", "waddle.test/forged", "other.test", "alice@waddle.test", "alice@waddle.test/other-device"])
    func wrongSenderCannotRejectKnownSend(sender: String) async throws {
        let (coordinator, _, _) = online()
        let id = try #require(await coordinator.send(Draft(text: "hi"), in: bobConversation))
        coordinator.handle(.deliveryAcked(stanzaID: id))
        coordinator.handle(.messageRejected(stanzaID: id, from: jid(sender), to: nil))
        #expect(coordinator.deliveries.state(of: id) == .acknowledged)
        #expect(coordinator.failedOutbound.isEmpty)
    }

    @Test func wrongRecipientOrUnknownIDCannotRejectKnownSend() async throws {
        let (coordinator, _, _) = online()
        let id = try #require(await coordinator.send(Draft(text: "hi"), in: bobConversation))
        coordinator.handle(.messageRejected(stanzaID: id, from: jid("bob@waddle.test"), to: jid("eve@waddle.test")))
        coordinator.handle(.messageRejected(stanzaID: "other-id", from: jid("bob@waddle.test"), to: nil))
        #expect(coordinator.deliveries.state(of: id) == .sent)
        #expect(coordinator.deliveries.state(of: "other-id") == nil)
        #expect(coordinator.failedOutbound.isEmpty)
    }

    @Test(arguments: ["waddle.test", "remote.test"])
    func trustedServicesCanReject(sender: String) async throws {
        let (coordinator, _, _) = online()
        let id = try #require(await coordinator.send(Draft(text: "hi"), in: .direct(bare("bob@remote.test"))))
        coordinator.handle(.messageRejected(stanzaID: id, from: jid(sender), to: nil))
        #expect(coordinator.deliveries.state(of: id) == .failed)
    }

    @Test(arguments: ["alice@waddle.test", "alice@waddle.test/other-device"])
    func ownAccountCanRejectOnlyItsOwnConversation(sender: String) async throws {
        let (coordinator, _, _) = online()
        let peerID = try #require(await coordinator.send(Draft(text: "peer"), in: .direct(bare("bob@remote.test"))))
        let selfID = try #require(await coordinator.send(Draft(text: "self"), in: .direct(me.jid)))
        coordinator.handle(.deliveryAcked(stanzaID: peerID))
        coordinator.handle(.deliveryAcked(stanzaID: selfID))
        coordinator.handle(.messageRejected(stanzaID: peerID, from: jid(sender), to: jid("alice@waddle.test/mac")))
        coordinator.handle(.messageRejected(stanzaID: selfID, from: jid(sender), to: jid("alice@waddle.test/mac")))
        #expect(coordinator.deliveries.state(of: peerID) == .acknowledged)
        #expect(coordinator.failedOutbound[peerID] == nil)
        #expect(coordinator.deliveries.state(of: selfID) == .failed)
        #expect(coordinator.failedOutbound[selfID] != nil)
    }

    @Test func roomOccupantCannotRejectRoomSendButRoomCan() async throws {
        let (coordinator, _, _) = online()
        let id = try #require(await coordinator.send(Draft(text: "hi"), in: roomConversation))
        coordinator.handle(.deliveryAcked(stanzaID: id))
        coordinator.handle(.messageRejected(stanzaID: id, from: jid("general@muc.waddle.test/eve"), to: nil))
        #expect(coordinator.deliveries.state(of: id) == .acknowledged)
        coordinator.handle(.messageRejected(stanzaID: id, from: jid(room.description), to: nil))
        #expect(coordinator.deliveries.state(of: id) == .failed)
    }

    @Test func transportFailureAfterAckDoesNotBecomeRejection() async throws {
        let (coordinator, _, _) = online()
        let id = try #require(await coordinator.send(Draft(text: "hi"), in: bobConversation))
        coordinator.handle(.deliveryAcked(stanzaID: id))
        coordinator.handle(.deliveryFailed(stanzaID: id))
        #expect(coordinator.deliveries.state(of: id) == .acknowledged)
        #expect(coordinator.failedOutbound.isEmpty)
    }
}
