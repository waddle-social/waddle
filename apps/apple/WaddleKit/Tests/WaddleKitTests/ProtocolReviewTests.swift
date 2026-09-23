import Foundation
import Testing
@testable import WaddleKit

/// Regressions for the protocol/bridge review: room occupancy (XEP-0045)
/// and failed archive queries (XEP-0313).
@MainActor
@Suite("Protocol review regressions")
struct ProtocolReviewTests {
    private let quiet = BareJID(localpart: "quiet", domain: "muc.waddle.test")!

    private func online(channels: [Channel]) -> (SessionCoordinator, FakePort) {
        let port = FakePort()
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.status.connection = .online
        coordinator.isSendReady = true
        coordinator.directory.apply(Topology(spaces: [], channels: channels))
        return (coordinator, port)
    }

    private func selfJoin(_ room: BareJID) -> WirePresence {
        WirePresence(
            from: jid("\(room)/alice"),
            kind: .available,
            occupant: .init(affiliation: .member, role: .participant, realJID: nil, statusCodes: [110])
        )
    }

    // MARK: - Occupancy

    @Test func openingANonAutojoinRoomJoinsIt() async {
        let (coordinator, port) = online(channels: [Channel(roomJID: quiet, name: "quiet", autojoin: false)])
        await coordinator.open(.room(quiet))
        #expect(port.joined == [quiet])
    }

    @Test func openingAJoinedRoomDoesNotRejoin() async {
        let (coordinator, port) = online(channels: [Channel(roomJID: room, name: "general")])
        coordinator.handle(.presence(selfJoin(room)))
        await coordinator.open(roomConversation)
        #expect(port.joined.isEmpty)
    }

    /// create_room's own join is answered while it configures the room,
    /// and its leave is echoed only after it returns: the room looks
    /// joined at that point but is about to be left.
    @Test func createdChannelIsRejoinedAfterCreateRoomLeaves() async throws {
        let (coordinator, port) = online(channels: [])
        let launch = BareJID(localpart: "launch", domain: "muc.waddle.test")!
        coordinator.handle(.presence(selfJoin(launch)))

        let created = try await coordinator.createChannel(name: "launch", summary: nil)
        #expect(created.jid == launch)
        #expect(port.joined == [launch])

        await coordinator.open(created)
        coordinator.handle(.presence(WirePresence(
            from: jid("\(launch)/alice"),
            kind: .unavailable,
            occupant: .init(affiliation: .owner, role: .none, realJID: nil, statusCodes: [110])
        )))
        coordinator.handle(.presence(selfJoin(launch)))
        #expect(coordinator.presence.joinedRooms.contains(launch))
    }

    @Test func openedRoomIsRejoinedAfterReconnect() async throws {
        let port = FakePort()
        port.topology = Topology(spaces: [], channels: [Channel(roomJID: quiet, name: "quiet", autojoin: false)])
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.start()
        port.emit(.connected)
        try await Task.sleep(nanoseconds: 100_000_000)
        #expect(port.joined.isEmpty)

        await coordinator.open(.room(quiet))
        #expect(port.joined == [quiet])

        port.emit(.disconnected)
        port.emit(.connected)
        try await Task.sleep(nanoseconds: 100_000_000)
        #expect(port.joined == [quiet, quiet])
        await coordinator.stop()
    }

    // MARK: - Failed archive queries

    @Test func failedFirstLoadIsRetryableNotTheBeginning() async {
        let (coordinator, port) = online(channels: [Channel(roomJID: room, name: "general")])
        port.failingHistoryRequests = 1
        await coordinator.loadLatest(roomConversation)
        var state = coordinator.history.state(of: roomConversation)
        #expect(state.failed)
        #expect(!state.hasLoadedLatest)

        // "Try again" in the header goes through loadOlder.
        port.historyPages = [ArchivePage(messages: [roomMessage("hi", from: "bob", stanzaID: "s1")], first: "s1", isComplete: false)]
        await coordinator.loadOlder(roomConversation)
        state = coordinator.history.state(of: roomConversation)
        #expect(!state.failed)
        #expect(state.hasLoadedLatest)
        #expect(state.olderCursor == "s1")
        #expect(port.historyRequests.map(\.1) == [nil, nil])
    }

    @Test func failedOlderPageKeepsPaging() async {
        let (coordinator, port) = online(channels: [Channel(roomJID: room, name: "general")])
        port.historyPages = [ArchivePage(messages: [roomMessage("new", from: "bob", stanzaID: "s9")], first: "s9", isComplete: false)]
        await coordinator.loadLatest(roomConversation)

        port.failingHistoryRequests = 1
        await coordinator.loadOlder(roomConversation)
        var state = coordinator.history.state(of: roomConversation)
        #expect(state.failed)
        #expect(state.hasMoreOlder)
        #expect(state.olderCursor == "s9")

        await coordinator.loadOlder(roomConversation)
        state = coordinator.history.state(of: roomConversation)
        #expect(!state.failed)
        #expect(port.historyRequests.map(\.1) == [nil, "s9", "s9"])
    }
}
