import Foundation
import Testing
@testable import WaddleKit

@MainActor
@Suite("Session stores")
struct StoreTests {
    private func entry(_ unread: Int, last: String, updated: Int64) -> InboxEntry {
        InboxEntry(partner: bob, kind: .direct, lastStanzaID: last, lastUpdated: updated, unread: unread, preview: nil)
    }

    @Test func inboxDropsStaleEntries() {
        let inbox = InboxStore()
        #expect(inbox.apply(entry(3, last: "m3", updated: 30)) != nil)
        #expect(inbox.apply(entry(1, last: "m1", updated: 10)) == nil)
        #expect(inbox.entry(for: bob)?.unread == 3)
    }

    @Test func inboxReadBarrierClampsRacingPush() {
        let inbox = InboxStore()
        _ = inbox.apply(entry(2, last: "m2", updated: 20))
        inbox.markRead(bob)
        // A push still naming the read message cannot resurrect the badge.
        #expect(inbox.apply(entry(2, last: "m2", updated: 21))?.unread == 0)
        // A genuinely newer message voids the barrier.
        #expect(inbox.apply(entry(3, last: "m3", updated: 22))?.unread == 3)
    }

    @Test func inboxRemembersAccountedIDs() {
        let inbox = InboxStore()
        _ = inbox.apply(entry(1, last: "m9", updated: 9))
        #expect(inbox.wasAccounted(bob, ids: ["m9", "x"]))
        #expect(!inbox.wasAccounted(bob, ids: ["x"]))
    }

    @Test func unreadSkipsActiveConversationAndOwnMessages() {
        let unread = UnreadStore()
        unread.liveMessage(in: bobConversation, isMine: false, mentionsMe: false)
        unread.liveMessage(in: bobConversation, isMine: true, mentionsMe: false)
        #expect(unread.count(for: bobConversation) == 1)
        unread.setActive(bobConversation)
        #expect(unread.count(for: bobConversation) == 0)
        unread.liveMessage(in: bobConversation, isMine: false, mentionsMe: false)
        unread.set(5, for: bobConversation)
        #expect(unread.count(for: bobConversation) == 0)
        unread.clearActive(ifMatches: roomConversation)
        #expect(unread.activeConversation == bobConversation)
    }

    @Test func deliveryAckBeforeOutcomeIsKept() {
        let deliveries = DeliveryStore()
        deliveries.acknowledged("c1")
        deliveries.began("c1")
        deliveries.outcome(.sent(stanzaID: "c1"), for: "c1")
        #expect(deliveries.state(of: "c1") == .acknowledged)
    }

    @Test func ackWinsOverTransportFailureButNotBounce() {
        let deliveries = DeliveryStore()
        deliveries.failed("c1")
        deliveries.acknowledged("c1")
        deliveries.began("c1")
        deliveries.outcome(.sent(stanzaID: "c1"), for: "c1")
        #expect(deliveries.state(of: "c1") == .acknowledged)
        deliveries.failed("c1")
        #expect(deliveries.state(of: "c1") == .acknowledged)
        deliveries.bounced("c1")
        #expect(deliveries.state(of: "c1") == .failed)
    }

    @Test func typingExpiresAndClearsOnMessage() {
        let typing = TypingStore(lifetime: 10)
        typing.apply(.composing, from: "bob", in: roomConversation, now: date(0))
        #expect(typing.names(in: roomConversation) == ["bob"])
        #expect(typing.sweep(now: date(5)))
        #expect(!typing.sweep(now: date(11)))
        #expect(typing.names(in: roomConversation).isEmpty)
        typing.apply(.composing, from: "bob", in: roomConversation, now: date(20))
        typing.messageArrived(from: "bob", in: roomConversation)
        #expect(typing.names(in: roomConversation).isEmpty)
    }

    @Test func presenceTracksSelfJoinAndBestResource() {
        let presence = PresenceStore(isRoom: { $0 == room })
        let join = WirePresence(
            from: jid("general@muc.waddle.test/alice"),
            kind: .available,
            occupant: .init(affiliation: .member, role: .moderator, realJID: nil, statusCodes: [110])
        )
        #expect(presence.apply(join) == .joined(room: room))
        #expect(presence.occupant(named: "alice", in: room)?.canModerate == true)

        presence.apply(WirePresence(from: jid("bob@waddle.test/a"), kind: .available, show: "away"))
        presence.apply(WirePresence(from: jid("bob@waddle.test/b"), kind: .available))
        #expect(presence.availability(of: bob) == .available)
        presence.apply(WirePresence(from: jid("bob@waddle.test/b"), kind: .unavailable))
        #expect(presence.availability(of: bob) == .away)
    }

    @Test func directListOrdersByRecency() {
        let directory = DirectoryStore()
        directory.touchDirect(bob, at: date(1), preview: "one")
        directory.touchDirect(bare("carol@waddle.test"), at: date(2), preview: "two")
        directory.touchDirect(bob, at: date(3), preview: "three")
        #expect(directory.directConversations.map(\.peer) == [bob, bare("carol@waddle.test")])
        // Older history never overwrites the newest preview.
        directory.touchDirect(bob, at: date(0), preview: "ancient")
        #expect(directory.directConversations.first?.preview == "three")
    }

    @Test func pinSeedDoesNotClobberLiveEvent() {
        let pins = PinStore()
        let version = pins.versionBeforeFetch(room)
        pins.apply(PinEvent(action: .pinned, targetStanzaID: "s9", by: nil, preview: nil), in: room)
        pins.seed([], in: room, fetchedAt: version)
        #expect(pins.isPinned("s9", in: room))
    }

    @Test func reconnectBackoffIsBoundedAndJittered() {
        let policy = ReconnectPolicy(base: 1, cap: 60)
        #expect(policy.delay(forAttempt: 0, unit: 0.5) == 1)
        #expect(policy.delay(forAttempt: 3, unit: 1) == 8)
        #expect(policy.delay(forAttempt: 20, unit: 1) == 60)
        #expect(policy.delay(forAttempt: 20, unit: 0) == 1)
    }

    @Test func roomLocalpartIsSlugged() {
        #expect(RoomLocalpart.make(from: "Product Launch 🚀!") == "product-launch")
        #expect(RoomLocalpart.make(from: "🚀") == nil)
    }
}
