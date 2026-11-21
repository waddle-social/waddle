import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

/// The admin console's spaces: the list, and making, changing and deleting
/// one, and the roles of its members.
@MainActor
@Suite("Admin console spaces")
struct AdminSpacesTests {
    private func online() -> (SessionCoordinator, FakePort) {
        let port = FakePort()
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.status.connection = .online
        return (coordinator, port)
    }

    private let design = AdminSpace(
        service: bare("spaces.waddle.test"), node: "design", name: "Design",
        description: "Pixels and critique", roomCount: 3, memberCount: 12
    )
    private let ops = AdminSpace(
        service: bare("spaces.waddle.test"), node: "ops", name: "Ops",
        description: nil, roomCount: 1, memberCount: 1
    )
    private let ada = AdminSpaceMember(jid: bare("ada@waddle.test"), role: .owner)
    private let bo = AdminSpaceMember(jid: bare("bo@waddle.test"), role: .member)

    // MARK: What a space may be called

    @Test func aDraftTrimsItsNameAndDescription() throws {
        let draft = try #require(AdminSpaceDraft(name: "  Design ", description: " Pixels\n"))

        #expect(draft.name == "Design")
        #expect(draft.description == "Pixels")
    }

    @Test func aBlankDescriptionIsNoDescription() throws {
        let draft = try #require(AdminSpaceDraft(name: "Design", description: "  "))

        #expect(draft.description == nil)
    }

    /// The server takes one to eighty characters.
    @Test func aSpaceNeedsANameTheServerWouldTake() {
        #expect(AdminSpaceDraft(name: "  ", description: "") == nil)
        #expect(AdminSpaceDraft(name: String(repeating: "a", count: 80), description: "") != nil)
        #expect(AdminSpaceDraft(name: String(repeating: "a", count: 81), description: "") == nil)
    }

    /// The server counts Unicode scalars; a family emoji is seven of them.
    @Test func aNameIsMeasuredTheWayTheServerMeasuresIt() {
        let family = "👨‍👩‍👧‍👦"
        #expect(AdminSpaceDraft(name: String(repeating: family, count: 11), description: "") != nil)
        #expect(AdminSpaceDraft(name: String(repeating: family, count: 12), description: "") == nil)
        #expect(AdminName.isTooLong(String(repeating: family, count: 12)))
        #expect(!AdminName.isTooLong(String(repeating: "a", count: 80) + " "))
    }

    @Test func removingMembersLowersTheCountButNotBelowNone() {
        #expect(design.removingMembers(2).memberCount == 10)
        #expect(ops.removingMembers(3).memberCount == 0)
        #expect(design.removingMembers(2).id == design.id)
    }

    // MARK: The list

    @Test func theSpacesListLoadsAndSearches() async {
        let (coordinator, port) = online()
        port.admin.spaces[.init()] = .success(AdminPage(rows: [design], nextCursor: "next"))
        port.admin.spaces[.init(cursor: "next")] = .success(AdminPage(rows: [ops], nextCursor: nil))
        port.admin.spaces[.init(prefix: "o")] = .success(AdminPage(rows: [ops], nextCursor: nil))
        let list = coordinator.adminSpacesList()

        await list.load()
        await list.loadMore()
        #expect(list.rows == [design, ops])

        await list.load(matching: "o")
        #expect(list.rows == [ops])
    }

    @Test func aRowTheServerChangedIsReplacedInPlace() async {
        let (coordinator, port) = online()
        port.admin.spaces[.init()] = .success(AdminPage(rows: [design, ops], nextCursor: nil))
        let list = coordinator.adminSpacesList()
        await list.load()
        let renamed = AdminSpace(
            service: design.service, node: design.node, name: "Studio",
            description: design.description, roomCount: 3, memberCount: 12
        )

        #expect(list.current(design) == design)
        list.replace(renamed)

        #expect(list.rows == [renamed, ops])
        #expect(list.current(design) == renamed)
        let unlisted = AdminSpace(
            service: design.service, node: "gone", name: "Gone", description: nil, roomCount: 0, memberCount: 0
        )
        #expect(list.current(unlisted) == unlisted)
    }

    @Test func aRowTheServerDeletedIsTakenOut() async {
        let (coordinator, port) = online()
        port.admin.spaces[.init()] = .success(AdminPage(rows: [design, ops], nextCursor: nil))
        let list = coordinator.adminSpacesList()
        await list.load()

        list.remove(design.id)

        #expect(list.rows == [ops])
    }

    // MARK: Making, changing, deleting

    @Test func creatingASpaceSendsItsDraft() async throws {
        let (coordinator, port) = online()
        let draft = try #require(AdminSpaceDraft(name: "Design", description: ""))

        try await coordinator.createAdminSpace(draft)

        #expect(port.admin.spaceWrites == [.create(draft)])
    }

    /// The answer names the space as the server now holds it; the counts
    /// are not part of the answer and stay as they were.
    @Test func changingASpaceAnswersWithWhatTheServerHolds() async throws {
        let (coordinator, port) = online()
        let draft = try #require(AdminSpaceDraft(name: "Studio", description: "New words"))

        let changed = try await coordinator.updateAdminSpace(design, to: draft)

        #expect(port.admin.spaceWrites == [.update(node: "design", draft)])
        #expect(changed.name == "Studio")
        #expect(changed.description == "New words")
        #expect(changed.roomCount == 3)
        #expect(changed.memberCount == 12)
        #expect(changed.id == design.id)
    }

    @Test func deletingASpaceAsksTheServer() async throws {
        let (coordinator, port) = online()

        try await coordinator.deleteAdminSpace(design)

        #expect(port.admin.spaceWrites == [.delete(node: "design")])
    }

    private let critique = bare("critique@muc.waddle.test")
    private let sketch = bare("sketch@muc.waddle.test")
    private let lounge = bare("lounge@muc.waddle.test")

    /// Two rooms in Design and one in Ops, all opened this session.
    private func withRooms() -> (SessionCoordinator, FakePort) {
        let (coordinator, port) = online()
        coordinator.directory.upsert(Channel(roomJID: critique, name: "critique", spaceID: "design"))
        coordinator.directory.upsert(Channel(roomJID: sketch, name: "sketch", spaceID: "design"))
        coordinator.directory.upsert(Channel(roomJID: lounge, name: "lounge", spaceID: "ops"))
        coordinator.onDemandRooms = [critique, sketch, lounge]
        return (coordinator, port)
    }

    /// The server deletes the rooms that belong to the space and unlinks
    /// the others. Joining a deleted room could make it again.
    @Test func aRoomOfADeletedSpaceIsNotJoinedUntilItIsListedAgain() async throws {
        let (coordinator, port) = withRooms()
        port.topology = Topology(spaces: [], channels: [
            Channel(roomJID: sketch, name: "sketch"),
            Channel(roomJID: lounge, name: "lounge", spaceID: "ops"),
        ])

        try await coordinator.deleteAdminSpace(design)

        #expect(coordinator.isFenced(critique))
        #expect(!coordinator.directory.isRoom(critique))
        #expect(!coordinator.onDemandRooms.contains(critique))
        #expect(!port.joined.contains(critique))
        #expect(!coordinator.isFenced(sketch))
        #expect(coordinator.directory.isRoom(sketch))
        #expect(!coordinator.isFenced(lounge))
        #expect(coordinator.onDemandRooms.contains(lounge))
    }

    /// Without a fresh look nothing says which rooms are left; a later
    /// look that lists one lets it be opened again.
    @Test func aRoomThatOutlivedItsSpaceCanBeOpenedOnceADiscoveryListsIt() async throws {
        let (coordinator, port) = withRooms()
        port.topologyFailure = .timeout
        try await coordinator.deleteAdminSpace(design)
        #expect(coordinator.isFenced(critique))
        #expect(coordinator.isFenced(sketch))
        #expect(!coordinator.isFenced(lounge))
        #expect(coordinator.onDemandRooms == [lounge])

        port.topologyFailure = nil
        port.topology = Topology(spaces: [], channels: [Channel(roomJID: sketch, name: "sketch")])
        await coordinator.refreshDirectory()

        #expect(!coordinator.isFenced(sketch))
        #expect(coordinator.isFenced(critique))
    }

    /// A discovery can leave out a room that is still there, so a room
    /// it does not list keeps what is known of it.
    @Test func aRoomMissingFromADiscoveryIsNotTakenAsDeleted() async throws {
        let (coordinator, port) = withRooms()
        port.topology = Topology(spaces: [], channels: [])

        try await coordinator.deleteAdminSpace(design)

        #expect(!coordinator.destroyedRooms.contains(critique))
        #expect(coordinator.isFenced(critique))
    }

    /// Making a room again under the name of one that went with its
    /// space is a room that is there.
    @Test func aRoomMadeAgainAfterItsSpaceWasDeletedIsJoined() async throws {
        let (coordinator, port) = withRooms()
        port.topology = Topology(spaces: [], channels: [])
        try await coordinator.deleteAdminSpace(design)
        #expect(coordinator.isFenced(critique))

        _ = try await coordinator.createChannel(name: "critique", summary: nil)

        #expect(!coordinator.isFenced(critique))
    }

    @Test func aDeleteTheServerRefusesFencesNoRooms() async {
        let (coordinator, port) = withRooms()
        port.admin.writeFailure = .rejected

        await #expect(throws: PortError.rejected) { try await coordinator.deleteAdminSpace(design) }

        #expect(!coordinator.isFenced(critique))
        #expect(coordinator.onDemandRooms == [critique, sketch, lounge])
    }

    /// A page asked for before a change does not know of it.
    @Test func anAnswerAskedForBeforeAChangeIsNotShownOverIt() async {
        let (coordinator, port) = online()
        port.admin.users[.init()] = .success(AdminPage(
            rows: [AdminUser(jid: ada.jid, displayName: nil, isOwner: false)], nextCursor: nil
        ))
        let list = coordinator.adminUsersList()
        await list.load()
        port.admin.usersFetches.isHolding = true
        let refresh = Task { await list.reload() }
        await eventually { port.admin.usersFetches.count == 1 }

        list.remove(ada.jid)
        port.admin.usersFetches.release()
        await refresh.value

        #expect(list.rows.isEmpty)
        #expect(!list.isLoading)
    }

    /// A change made while a search is on its way keeps the search.
    @Test func aChangeDuringASearchIsMadeOverItsAnswer() async {
        let (coordinator, port) = online()
        let bo = AdminUser(jid: self.bo.jid, displayName: nil, isOwner: false)
        let boNamed = AdminUser(jid: self.bo.jid, displayName: "Bo", isOwner: false)
        port.admin.users[.init()] = .success(AdminPage(rows: [bo], nextCursor: nil))
        port.admin.users[.init(prefix: "b")] = .success(AdminPage(rows: [bo], nextCursor: nil))
        let list = coordinator.adminUsersList()
        await list.load()
        port.admin.usersFetches.isHolding = true
        let search = Task { await list.load(matching: "b") }
        await eventually { port.admin.usersFetches.count == 1 }

        list.replace(boNamed)
        port.admin.usersFetches.release()
        await search.value

        #expect(list.rows == [boNamed])
        #expect(list.query == "b")
        #expect(!list.isLoading)
    }

    /// Home and the sidebar show spaces from the directory.
    @Test func everyChangeToASpaceIsFollowedByAFreshDirectory() async throws {
        let (coordinator, port) = online()
        let draft = try #require(AdminSpaceDraft(name: "Studio", description: ""))
        port.topology = Topology(
            spaces: [Space(id: "studio", serviceJID: nil, name: "Studio", summary: nil)], channels: []
        )

        try await coordinator.createAdminSpace(draft)
        #expect(coordinator.directory.spaces.map(\.name) == ["Studio"])

        _ = try await coordinator.updateAdminSpace(design, to: draft)
        try await coordinator.deleteAdminSpace(ops)
        #expect(port.topologyRequests == 3)
    }

    /// Joining a room again makes the server send everyone in it again.
    @Test func aChangeToASpaceDoesNotJoinRoomsAlreadyJoined() async throws {
        let (coordinator, port) = online()
        port.topology = Topology(spaces: [], channels: [
            Channel(roomJID: lounge, name: "lounge"),
            Channel(roomJID: sketch, name: "sketch"),
        ])
        coordinator.handle(.presence(WirePresence(
            from: jid("\(lounge)/alice"),
            kind: .available,
            occupant: .init(affiliation: .member, role: .participant, realJID: nil, statusCodes: [110])
        )))
        #expect(coordinator.presence.joinedRooms.contains(lounge))
        let draft = try #require(AdminSpaceDraft(name: "Studio", description: ""))

        try await coordinator.createAdminSpace(draft)

        #expect(port.joined == [sketch])
    }

    /// A reconnect does not cancel the task a space was changed from.
    @Test func aDirectoryAskedForBeforeAReconnectIsNotShownOverTheNewOne() async throws {
        let (coordinator, port) = online()
        let draft = try #require(AdminSpaceDraft(name: "Studio", description: ""))
        port.topology = Topology(spaces: [], channels: [Channel(roomJID: sketch, name: "sketch")])
        port.holdsTopology = true
        let creating = Task { try await coordinator.createAdminSpace(draft) }
        await eventually { port.topologyRequests == 1 }

        coordinator.connectionEpoch += 1
        coordinator.directory.apply(Topology(spaces: [], channels: [Channel(roomJID: lounge, name: "lounge")]))
        port.releaseTopology()
        try await creating.value

        #expect(coordinator.directory.isRoom(lounge))
        #expect(!coordinator.directory.isRoom(sketch))
        #expect(port.joined.isEmpty)
    }

    @Test func aRefusedChangeAsksForNoDirectory() async throws {
        let (coordinator, port) = online()
        port.admin.writeFailure = .rejected
        let draft = try #require(AdminSpaceDraft(name: "Studio", description: ""))

        await #expect(throws: PortError.rejected) { try await coordinator.createAdminSpace(draft) }

        #expect(port.topologyRequests == 0)
    }

    @Test func aChangeTheServerRefusesIsThrown() async throws {
        let (coordinator, port) = online()
        port.admin.writeFailure = .rejected
        let draft = try #require(AdminSpaceDraft(name: "Design", description: ""))

        await #expect(throws: PortError.rejected) { try await coordinator.createAdminSpace(draft) }
        await #expect(throws: PortError.rejected) { try await coordinator.deleteAdminSpace(design) }
    }

    @Test func nothingIsChangedWhileOffline() async throws {
        let port = FakePort()
        let coordinator = SessionCoordinator(account: me, port: port)
        let draft = try #require(AdminSpaceDraft(name: "Design", description: ""))

        await #expect(throws: PortError.notConnected) { try await coordinator.createAdminSpace(draft) }
        await #expect(throws: PortError.notConnected) { try await coordinator.deleteAdminSpace(design) }
        await #expect(throws: PortError.notConnected) {
            try await coordinator.setAdminSpaceRole(.admin, of: bo.jid, in: design)
        }
        #expect(port.admin.spaceWrites.isEmpty)
    }

    // MARK: Members

    @Test func theMembersOfASpaceLoadAPageAtATime() async {
        let (coordinator, port) = online()
        port.admin.members[.init(prefix: "design")] = .success(AdminPage(rows: [ada], nextCursor: "next"))
        port.admin.members[.init(prefix: "design", cursor: "next")] = .success(AdminPage(rows: [bo], nextCursor: nil))
        let list = coordinator.adminSpaceMembersList(of: design)

        await list.load()
        await list.loadMore()

        #expect(list.rows == [ada, bo])
    }

    /// The answer names the role the server gave, which is what is shown.
    @Test func changingARoleAnswersWithTheRoleGiven() async throws {
        let (coordinator, port) = online()

        let changed = try await coordinator.setAdminSpaceRole(.admin, of: bo.jid, in: design)

        #expect(port.admin.spaceWrites == [.setRole(node: "design", member: bo.jid, .admin)])
        #expect(changed == AdminSpaceMember(jid: bo.jid, role: .admin))
    }
}
