import Foundation
import Testing

@testable import WaddleKit
@testable import WaddleKitTestSupport

/// A room's call announcement in the timeline, and the fastening that ends it.
@MainActor struct CallThreadTests {
    private func store() -> TimelineStore {
        let store = TimelineStore()
        store.account = me
        return store
    }

    private func anchor(origin: String = "o1", stanza: String = "s1", from: JID? = nil, thread: String = "t1") -> WireMessage {
        var message = WireMessage(
            type: .groupchat,
            from: from ?? jid(room.description),
            to: JID(bare: me.jid, resource: "phone"),
            identity: MessageIdentity(
                messageID: "m-\(origin)", originID: origin, stanzaIDs: [StanzaID(id: stanza, by: room)]),
            body: "ada started a call",
            thread: thread
        )
        message.callThread = CallThreadAnchor(kind: .room, hasVideo: true)
        return message
    }

    private func ended(_ anchorID: String = "o1", from: JID? = nil, duration: TimeInterval? = 300) -> WireMessage {
        var message = WireMessage(
            type: .groupchat, from: from ?? jid(room.description), to: JID(bare: me.jid, resource: "phone"),
            identity: MessageIdentity(messageID: "e-\(anchorID)", stanzaIDs: [StanzaID(id: "es-\(anchorID)", by: room)])
        )
        message.callThreadEnded = CallThreadEnded(anchorID: anchorID, duration: duration)
        return message
    }

    private func items(_ store: TimelineStore) -> [TimelineItem] {
        store.timeline(for: roomConversation).items
    }

    @Test func theAnnouncementIsARowInTheFeedThoughItHeadsAThread() {
        let store = store()
        store.ingest(anchor())
        #expect(items(store).count == 1)
        #expect(items(store)[0].callAnchor?.hasVideo == true)
        #expect(items(store)[0].isFeedVisible)
        #expect(items(store)[0].callEnded == nil)
    }

    @Test func theRoomSayingItIsOverMarksTheAnnouncement() {
        let store = store()
        store.ingest(anchor())
        let result = store.ingest(ended())
        #expect(result == .mutation)
        #expect(items(store).count == 1, "the fastening is not a row")
        #expect(items(store)[0].callEnded?.duration == 300)
    }

    /// History is paged newest first: the end is often read before the beginning.
    @Test func anEndReadBeforeItsAnnouncementIsKeptForIt() {
        let store = store()
        store.ingest(ended())
        store.ingest(anchor())
        #expect(items(store).first?.callEnded != nil)
    }

    @Test func eachEndFindsItsOwnAnnouncement() {
        let store = store()
        store.ingest(anchor(origin: "o1", stanza: "s1", thread: "t1"))
        store.ingest(anchor(origin: "o2", stanza: "s2", thread: "t2"))
        store.ingest(ended("o2"))
        #expect(items(store).map { $0.callEnded != nil } == [false, true])
    }

    @Test func anOccupantCannotAnnounceACallOrEndOne() {
        let store = store()
        let mallory = jid("\(room)/mallory")
        store.ingest(anchor(origin: "fake", stanza: "s9", from: mallory))
        store.ingest(anchor())
        store.ingest(ended("o1", from: mallory))
        let forged = items(store).first { $0.identity.originID == "fake" }
        #expect(forged != nil)
        #expect(forged?.callAnchor == nil, "an occupant's message marked as an announcement is an ordinary message")
        #expect(forged?.isFeedVisible == false, "and, naming a thread it does not head, a thread reply")
        #expect(items(store).first { $0.identity.originID == "o1" }?.callEnded == nil, "an occupant's fastening ends nothing")
    }

    @Test func aPeerCannotAnnounceACallInAConversationOfTwo() {
        let store = store()
        let peer = jid("ada@waddle.test/web")
        var message = WireMessage(
            type: .chat, from: peer, to: JID(bare: me.jid, resource: "phone"),
            identity: MessageIdentity(messageID: "m1", originID: "o1"), body: "hello", thread: "o1"
        )
        message.callThread = CallThreadAnchor(kind: .direct, hasVideo: false)
        store.ingest(message)
        let row = store.timeline(for: .direct(peer.bare)).items.first
        #expect(row != nil)
        #expect(row?.callAnchor == nil)
    }

    @Test(arguments: [
        ("PT1H2M3S", 3723.0), ("PT5M0S", 300.0), ("PT42S", 42.0), ("PT2H", 7200.0), ("PT0S", 0.0),
    ])
    func theServersDurationsAreRead(_ iso: String, _ seconds: Double) {
        #expect(CallThreadEnded.seconds(iso: iso) == seconds)
    }

    @Test(arguments: ["", "PT", "5M", "PT5", "PT5X", "PTM", "PT5S5M", "PT5M5M", "P1D", "PT-5S"])
    func anythingElseIsNoDuration(_ iso: String) {
        #expect(CallThreadEnded.seconds(iso: iso) == nil)
    }
}
