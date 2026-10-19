import Foundation
import Testing
@testable import WaddleKit

private let dave = bare("dave@waddle.test")
private let erin = bare("erin@waddle.test")
private let frank = bare("frank@waddle.test")
private let otherRoom = bare("random@muc.waddle.test")

private func occupant(_ nick: String, realJID: BareJID?) -> Occupant {
    Occupant(nick: nick, availability: .available, status: nil, affiliation: .member, role: .participant, realJID: realJID, hats: [])
}

private func occupantPresence(_ nick: String, realJID: BareJID?, codes: Set<Int> = []) -> WirePresence {
    WirePresence(
        from: room.with(resource: nick)!,
        kind: .available,
        occupant: .init(affiliation: .member, role: .participant, realJID: realJID.map { jid("\($0)/phone") }, statusCodes: codes)
    )
}

/// `@` mentions in rooms reach people with no MUC presence through the
/// XEP-0055 user directory: popover results, and the send-time directory.
@Suite("Mention directory search")
struct MentionDirectorySearchTests {
    @Test func usernameFallsBackToLocalpart() {
        #expect(UserSearchResult(jid: erin, username: "erin.k", displayName: nil).username == "erin.k")
        #expect(UserSearchResult(jid: erin, username: "  ", displayName: nil).username == "erin")
        #expect(UserSearchResult(jid: erin, displayName: "Erin").username == "erin")
    }

    @Test func additionsSkipListedOwnAndDuplicateUsers() {
        let found = [
            UserSearchResult(jid: frank, username: "frank", displayName: nil),
            UserSearchResult(jid: dave, username: "dave", displayName: nil),
            UserSearchResult(jid: me.jid, username: "alice", displayName: nil),
            UserSearchResult(jid: erin, username: "erin", displayName: nil),
            UserSearchResult(jid: frank, username: "frank", displayName: nil),
        ]

        let additions = MentionDirectorySearch.additions(found, listed: [dave], ownJID: me.jid)

        #expect(additions.map(\.jid) == [frank, erin])
    }

    @Test func cachesPerRoomAndQuery() {
        var search = MentionDirectorySearch()
        let erinResult = UserSearchResult(jid: erin, username: "erin", displayName: nil)

        search.record([erinResult], for: " Er ", in: room)

        #expect(search.cached("er", in: room) == [erinResult])
        #expect(search.cached("ER", in: room) == [erinResult])
        #expect(search.cached("er", in: otherRoom) == nil)
        #expect(search.cached("e", in: room) == nil)
        #expect(search.seenUsers(in: room) == [erinResult])
        #expect(search.seenUsers(in: otherRoom).isEmpty)

        search.clear()
        #expect(search.cached("er", in: room) == nil)
        #expect(search.seenUsers(in: room).isEmpty)
    }

    @Test func roomDirectoryAddsDirectoryUsersAfterOccupants() {
        let directory = MentionDirectory.room(
            occupants: [occupant("alice", realJID: me.jid), occupant("Dave", realJID: dave), occupant("ghost", realJID: nil)],
            ownNick: "alice",
            directoryUsers: [
                UserSearchResult(jid: frank, username: "frank", displayName: nil),
                // Collides with the occupant nick "Dave": the occupant wins.
                UserSearchResult(jid: bare("impostor@waddle.test"), username: "dave", displayName: nil),
                UserSearchResult(jid: me.jid, username: "alice", displayName: nil),
                UserSearchResult(jid: erin, username: "erin", displayName: nil),
            ],
            ownJID: me.jid
        )

        #expect(directory == MentionDirectory(
            targets: [
                MentionDirectory.Target(label: "Dave", jid: dave),
                MentionDirectory.Target(label: "erin", jid: erin),
                MentionDirectory.Target(label: "frank", jid: frank),
            ],
            broadcasts: true
        ))
    }
}

@MainActor
@Suite("Mention directory search in a session")
struct SessionMentionDirectorySearchTests {
    private func coordinator(_ port: FakePort) -> SessionCoordinator {
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.status.connection = .online
        coordinator.isSendReady = true
        coordinator.directory.apply(Topology(spaces: [], channels: [Channel(roomJID: room, name: "general")]))
        coordinator.handle(.presence(occupantPresence("alice", realJID: me.jid, codes: [110])))
        coordinator.handle(.presence(occupantPresence("dave", realJID: dave)))
        return coordinator
    }

    @Test func searchesOncePerRoomAndQuery() async throws {
        let port = FakePort()
        let erinResult = UserSearchResult(jid: erin, username: "erin", displayName: "Erin")
        port.userSearchResults["er"] = [erinResult]
        let coordinator = coordinator(port)

        #expect(try await coordinator.searchMentionableUsers("er", in: room) == [erinResult])
        #expect(try await coordinator.searchMentionableUsers("er", in: room) == [erinResult])
        #expect(coordinator.cachedMentionableUsers("er", in: room) == [erinResult])
        #expect(port.userSearchRequests == ["er"])

        _ = try await coordinator.searchMentionableUsers("er", in: otherRoom)
        #expect(port.userSearchRequests == ["er", "er"])
    }

    @Test func emptyQueryDoesNotSearch() async throws {
        let port = FakePort()
        let coordinator = coordinator(port)

        #expect(try await coordinator.searchMentionableUsers("  ", in: room).isEmpty)
        #expect(port.userSearchRequests.isEmpty)
    }

    @Test func failedSearchIsNotCached() async {
        let port = FakePort()
        port.failsUserSearch = true
        let coordinator = coordinator(port)

        await #expect(throws: PortError.self) {
            try await coordinator.searchMentionableUsers("er", in: room)
        }
        #expect(coordinator.cachedMentionableUsers("er", in: room) == nil)
    }

    @Test func roomSendAddressesDirectoryUsersSeenInThatRoom() async throws {
        let port = FakePort()
        port.userSearchResults["e"] = [
            UserSearchResult(jid: erin, username: "erin", displayName: nil),
            UserSearchResult(jid: bare("impostor@waddle.test"), username: "DAVE", displayName: nil),
        ]
        let coordinator = coordinator(port)
        _ = try await coordinator.searchMentionableUsers("e", in: room)

        await coordinator.send(Draft(text: "@erin hi"), in: roomConversation)

        #expect(port.sent.last?.options.mentionDirectory == MentionDirectory(
            targets: [
                MentionDirectory.Target(label: "dave", jid: dave),
                MentionDirectory.Target(label: "erin", jid: erin),
            ],
            broadcasts: true
        ))
    }

    @Test func directoryUsersStayInTheirRoomAndOutOfDirectMessages() async throws {
        let port = FakePort()
        port.userSearchResults["e"] = [UserSearchResult(jid: erin, username: "erin", displayName: nil)]
        let coordinator = coordinator(port)
        _ = try await coordinator.searchMentionableUsers("e", in: otherRoom)

        await coordinator.send(Draft(text: "@erin hi"), in: roomConversation)
        #expect(port.sent.last?.options.mentionDirectory == MentionDirectory(
            targets: [MentionDirectory.Target(label: "dave", jid: dave)],
            broadcasts: true
        ))

        await coordinator.send(Draft(text: "@erin hi"), in: bobConversation)
        #expect(port.sent.last?.options.mentionDirectory == nil)
    }
}
