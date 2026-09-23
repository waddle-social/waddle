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
