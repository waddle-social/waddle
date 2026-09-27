import Foundation
import Testing
@testable import WaddleKit

private let dave = bare("dave@waddle.test")
private let erin = bare("erin@waddle.test")

private func occupantPresence(_ nick: String, realJID: BareJID?, kind: WirePresence.Kind = .available) -> WirePresence {
    WirePresence(
        from: room.with(resource: nick)!,
        kind: kind,
        occupant: .init(affiliation: .member, role: .participant, realJID: realJID.map { jid("\($0)/phone") }, statusCodes: [])
    )
}

@MainActor
@Suite("Room author resolution")
struct RoomAuthorTests {
    private func coordinator() -> SessionCoordinator {
        let coordinator = SessionCoordinator(account: me, port: FakePort())
        coordinator.status.connection = .online
        coordinator.directory.apply(Topology(spaces: [], channels: [Channel(roomJID: room, name: "general")]))
        return coordinator
    }

    private func row(_ coordinator: SessionCoordinator, _ stanzaID: String) -> TimelineItem? {
        coordinator.timelines.timeline(for: roomConversation).items.first { $0.id == stanzaID }
    }

    @Test func archivedRowUsesItsArchivedRealJID() {
        let coordinator = coordinator()
        var archived = roomMessage("hi", from: "dave", stanzaID: "s1", at: date(1), source: .archive(mamID: "s1"))
        archived.authorRealJID = dave
        coordinator.timelines.ingest(archived)
        #expect(coordinator.authorJID(of: row(coordinator, "s1")!) == dave)
    }

    @Test func departedAuthorKeepsTheirJID() {
        let coordinator = coordinator()
        coordinator.handle(.presence(occupantPresence("dave", realJID: dave)))
        coordinator.handle(.message(roomMessage("hi", from: "dave", stanzaID: "s1")))
        coordinator.handle(.presence(occupantPresence("dave", realJID: dave, kind: .unavailable)))
        #expect(coordinator.presence.occupant(named: "dave", in: room) == nil)
        #expect(coordinator.authorJID(of: row(coordinator, "s1")!) == dave)

        // A page loaded after they left, from an archive without real JIDs.
        coordinator.timelines.ingest(roomMessage("earlier", from: "dave", stanzaID: "s0", at: date(0), source: .archive(mamID: "s0")))
        #expect(coordinator.authorJID(of: row(coordinator, "s0")!) == dave)
    }

    @Test func mappingSurvivesReconnect() {
        let coordinator = coordinator()
        coordinator.handle(.presence(occupantPresence("dave", realJID: dave)))
        coordinator.handle(.disconnected)
        coordinator.timelines.ingest(roomMessage("hi", from: "dave", stanzaID: "s1", at: date(1), source: .archive(mamID: "s1")))
        #expect(coordinator.authorJID(of: row(coordinator, "s1")!) == dave)
    }

    @Test func liveRowKeepsItsSenderWhenTheNickChangesHands() {
        let coordinator = coordinator()
        coordinator.handle(.presence(occupantPresence("sam", realJID: dave)))
        coordinator.handle(.message(roomMessage("from dave", from: "sam", stanzaID: "s1")))
        coordinator.handle(.presence(occupantPresence("sam", realJID: dave, kind: .unavailable)))
        coordinator.handle(.presence(occupantPresence("sam", realJID: erin)))
        coordinator.handle(.message(roomMessage("from erin", from: "sam", stanzaID: "s2")))
        #expect(coordinator.authorJID(of: row(coordinator, "s1")!) == dave)
        #expect(coordinator.authorJID(of: row(coordinator, "s2")!) == erin)
    }

    @Test func liveCopyKeepsTheArchivedRealJID() {
        let coordinator = coordinator()
        var archived = roomMessage("hi", from: "dave", stanzaID: "s1", at: date(1), source: .archive(mamID: "s1"))
        archived.authorRealJID = dave
        coordinator.timelines.ingest(archived)
        coordinator.handle(.message(roomMessage("hi", from: "dave", stanzaID: "s1")))
        #expect(row(coordinator, "s1")?.message.source == .live)
        #expect(coordinator.authorJID(of: row(coordinator, "s1")!) == dave)
    }

    @Test func delayedReplayAfterAHandoverShowsInitials() {
        let coordinator = coordinator()
        coordinator.handle(.presence(occupantPresence("sam", realJID: dave)))
        coordinator.handle(.presence(occupantPresence("sam", realJID: dave, kind: .unavailable)))
        coordinator.handle(.presence(occupantPresence("sam", realJID: erin)))
        // Dave's message, redelivered with a XEP-0203 delay after Erin took the nick.
        coordinator.handle(.message(roomMessage("from dave", from: "sam", stanzaID: "s1", at: date(1))))
        #expect(row(coordinator, "s1")?.message.authorRealJID == nil)
        #expect(coordinator.authorJID(of: row(coordinator, "s1")!) == nil)
    }

    @Test func delayedReplayKeepsTheArchivedJID() {
        let coordinator = coordinator()
        var archived = roomMessage("from dave", from: "sam", stanzaID: "s1", at: date(1), source: .archive(mamID: "s1"))
        archived.authorRealJID = dave
        coordinator.timelines.ingest(archived)
        coordinator.handle(.presence(occupantPresence("sam", realJID: erin)))
        coordinator.handle(.message(roomMessage("from dave", from: "sam", stanzaID: "s1", at: date(1))))
        #expect(row(coordinator, "s1")?.message.source == .live)
        #expect(coordinator.authorJID(of: row(coordinator, "s1")!) == dave)
    }

    @Test func undelayedLiveMessageIsStampedWithThePresentOccupant() {
        let coordinator = coordinator()
        coordinator.handle(.presence(occupantPresence("sam", realJID: erin)))
        coordinator.handle(.message(roomMessage("now", from: "sam", stanzaID: "s1")))
        #expect(row(coordinator, "s1")?.message.authorRealJID == erin)
    }

    @Test func firstStampSurvivesALaterCopy() {
        let coordinator = coordinator()
        var archived = roomMessage("hi", from: "sam", stanzaID: "s1", at: date(1), source: .archive(mamID: "s1"))
        archived.authorRealJID = dave
        coordinator.timelines.ingest(archived)
        var live = roomMessage("hi", from: "sam", stanzaID: "s1", at: date(1))
        live.authorRealJID = erin
        coordinator.timelines.ingest(live)
        #expect(row(coordinator, "s1")?.message.source == .live)
        #expect(row(coordinator, "s1")?.message.authorRealJID == dave)
    }

    @Test func nickIsNeverTurnedIntoAJID() {
        let coordinator = coordinator()
        // `bob` is a known contact, but nothing ties the room nick to him.
        coordinator.directory.touchDirect(bob, at: date(0), preview: nil)
        coordinator.handle(.message(roomMessage("hi", from: "bob", stanzaID: "s1")))
        #expect(coordinator.authorJID(of: row(coordinator, "s1")!) == nil)
    }

    @Test func directRowUsesTheSender() {
        let coordinator = coordinator()
        coordinator.handle(.message(directMessage("hi", from: jid("bob@waddle.test/a"), to: jid("alice@waddle.test/phone"), id: "d1")))
        let item = coordinator.timelines.timeline(for: bobConversation).items[0]
        #expect(coordinator.authorJID(of: item) == bob)
    }

    @Test func signOutForgetsTheMapping() async {
        let coordinator = coordinator()
        coordinator.handle(.presence(occupantPresence("dave", realJID: dave)))
        await coordinator.stop()
        #expect(coordinator.presence.realJID(ofNick: "dave", in: room) == nil)
    }

    @Test func bodylessHeadlineCreatesNoRow() {
        let coordinator = coordinator()
        let event = WireMessage(
            type: .headline,
            from: jid("bob@waddle.test"),
            to: jid("alice@waddle.test/phone"),
            identity: MessageIdentity(messageID: "pep1", originID: nil, stanzaID: nil, stanzaIDs: [])
        )
        coordinator.handle(.message(event))
        #expect(coordinator.timelines.timeline(for: bobConversation).items.isEmpty)
        #expect(coordinator.directory.directConversations.isEmpty)
    }
}
