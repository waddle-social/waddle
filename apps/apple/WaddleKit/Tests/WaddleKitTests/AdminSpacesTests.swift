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
        #expect(AdminSpaceDraft.isNameTooLong(String(repeating: family, count: 12)))
        #expect(!AdminSpaceDraft.isNameTooLong(String(repeating: "a", count: 80) + " "))
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

        list.replace(renamed)

        #expect(list.rows == [renamed, ops])
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

    private let critique = bare("critique@rooms.waddle.test")
    private let sketch = bare("sketch@rooms.waddle.test")
    private let lounge = bare("lounge@rooms.waddle.test")

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
    @Test func roomsDeletedWithTheirSpaceAreGoneAndNeverJoinedAgain() async throws {
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

    /// Without a fresh look, nothing says which rooms are left.
    @Test func roomsOfADeletedSpaceStayUnjoinedWhenDiscoveryFails() async throws {
        let (coordinator, port) = withRooms()
        port.topologyFailure = .timeout

        try await coordinator.deleteAdminSpace(design)

        #expect(coordinator.isFenced(critique))
        #expect(coordinator.isFenced(sketch))
        #expect(!coordinator.isFenced(lounge))
        #expect(coordinator.onDemandRooms == [lounge])
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
