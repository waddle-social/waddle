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
        address: "spaces.waddle.test", node: "design", name: "Design",
        description: "Pixels and critique", roomCount: 3, memberCount: 12
    )
    private let ops = AdminSpace(
        address: "spaces.waddle.test", node: "ops", name: "Ops",
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
            address: design.address, node: design.node, name: "Studio",
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
