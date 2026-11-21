import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

/// The admin console's rooms: the list, making, changing and deleting one,
/// who is there, and the roles people hold in it.
@MainActor
@Suite("Admin console rooms")
struct AdminRoomsTests {
    private func online() -> (SessionCoordinator, FakePort) {
        let port = FakePort()
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.status.connection = .online
        return (coordinator, port)
    }

    private let lounge = AdminRoom(
        jid: bare("lounge@muc.waddle.test"), name: "Lounge", description: "Small talk",
        kind: .text, isListed: true, isMembersOnly: false, presentCount: 4, memberCount: 9
    )
    private let board = AdminRoom(
        jid: bare("board@muc.waddle.test"), name: "Board", description: nil,
        kind: .announcement, isListed: false, isMembersOnly: true, presentCount: 0, memberCount: 3
    )
    private let ada = bare("ada@waddle.test")
    private let bo = bare("bo@waddle.test")

    private func draft(
        _ name: String = "Lounge", description: String = "", kind: AdminRoomKind = .text,
        isListed: Bool = true, isMembersOnly: Bool = false, space: AdminSpace? = nil
    ) throws -> AdminRoomDraft {
        try #require(AdminRoomDraft(
            name: name, description: description, kind: kind,
            isListed: isListed, isMembersOnly: isMembersOnly, space: space
        ))
    }

    // MARK: What a room may be

    @Test func aDraftTrimsItsNameAndDescription() throws {
        let draft = try draft("  Lounge ", description: " Small talk\n")

        #expect(draft.name == "Lounge")
        #expect(draft.description == "Small talk")
        #expect(try self.draft(description: "  ").description == nil)
    }

    @Test func aRoomNeedsANameTheServerWouldTake() {
        #expect(AdminRoomDraft(name: " ", description: "", kind: .text, isListed: true, isMembersOnly: false, space: nil) == nil)
        #expect(AdminRoomDraft(
            name: String(repeating: "a", count: 81), description: "", kind: .text,
            isListed: true, isMembersOnly: false, space: nil
        ) == nil)
    }

    /// The server has rooms that are hidden and open to anyone with the
    /// address; a draft says what it is given and decides nothing.
    @Test func aDraftKeepsTheAccessItIsGiven() throws {
        #expect(try !draft(isListed: false, isMembersOnly: false).isMembersOnly)
        #expect(try draft(isListed: true, isMembersOnly: true).isMembersOnly)
    }

    /// The server makes these three; any other kind is its own business.
    @Test func onlyTheKindsTheServerMakesCanBeChosen() {
        #expect(AdminRoomKind.choices == [.text, .announcement, .forum])
        #expect(AdminRoomKind.text.canBeChosen)
        #expect(!AdminRoomKind.other.canBeChosen)
    }

    // MARK: The list

    @Test func theRoomsListLoadsAndSearches() async {
        let (coordinator, port) = online()
        port.admin.rooms[.init()] = .success(AdminPage(rows: [lounge], nextCursor: "next"))
        port.admin.rooms[.init(cursor: "next")] = .success(AdminPage(rows: [board], nextCursor: nil))
        port.admin.rooms[.init(prefix: "b")] = .success(AdminPage(rows: [board], nextCursor: nil))
        let list = coordinator.adminRoomsList()

        await list.load()
        await list.loadMore()
        #expect(list.rows == [lounge, board])

        await list.load(matching: "b")
        #expect(list.rows == [board])
    }

    // MARK: Making, changing, deleting

    @Test func creatingARoomSendsItsDraftAndLooksAtTheDirectoryAgain() async throws {
        let (coordinator, port) = online()
        let draft = try draft()

        try await coordinator.createAdminRoom(draft)

        #expect(port.admin.roomWrites == [.create(draft)])
        #expect(port.topologyRequests == 1)
    }

    /// The answer has no counts; they stay as they were.
    @Test func changingARoomAnswersWithWhatTheServerHolds() async throws {
        let (coordinator, port) = online()
        let draft = try draft("Salon", description: "Chat", kind: .forum, isListed: false)

        let changed = try await coordinator.updateAdminRoom(lounge, to: draft)

        #expect(port.admin.roomWrites == [.update(lounge.jid, draft)])
        #expect(changed.name == "Salon")
        #expect(changed.kind == .forum)
        #expect(!changed.isListed)
        #expect(!changed.isMembersOnly)
        #expect(changed.presentCount == 4)
        #expect(changed.memberCount == 9)
        #expect(port.topologyRequests == 1)
    }

    /// As with a room deleted from its details: never joined again this
    /// session, since joining could make it again.
    @Test func aDeletedRoomIsForgottenAndNeverJoinedAgain() async throws {
        let (coordinator, port) = online()
        coordinator.directory.upsert(Channel(roomJID: lounge.jid, name: "Lounge"))
        coordinator.onDemandRooms = [lounge.jid]

        try await coordinator.deleteAdminRoom(lounge)

        #expect(port.admin.roomWrites == [.delete(lounge.jid)])
        #expect(coordinator.isFenced(lounge.jid))
        #expect(!coordinator.directory.isRoom(lounge.jid))
        #expect(coordinator.onDemandRooms.isEmpty)
    }

    @Test func aRoomTheServerRefusesToDeleteStaysJoinable() async throws {
        let (coordinator, port) = online()
        port.admin.writeFailure = .rejected
        coordinator.directory.upsert(Channel(roomJID: lounge.jid, name: "Lounge"))

        await #expect(throws: PortError.rejected) { try await coordinator.deleteAdminRoom(lounge) }

        #expect(!coordinator.isFenced(lounge.jid))
        #expect(coordinator.directory.isRoom(lounge.jid))
    }

    @Test func nothingIsChangedWhileOffline() async throws {
        let port = FakePort()
        let coordinator = SessionCoordinator(account: me, port: port)
        let draft = try draft()

        await #expect(throws: PortError.notConnected) { try await coordinator.createAdminRoom(draft) }
        await #expect(throws: PortError.notConnected) { try await coordinator.deleteAdminRoom(lounge) }
        await #expect(throws: PortError.notConnected) { try await coordinator.removeFromAdminRoom(ada, room: lounge) }
        await #expect(throws: PortError.notConnected) {
            try await coordinator.setAdminRoomAffiliation(.admin, of: bo, in: lounge)
        }
        #expect(port.admin.roomWrites.isEmpty)
    }

    // MARK: Who is there, and roles

    @Test func whoIsInARoomLoadsAPageAtATime() async {
        let (coordinator, port) = online()
        let adaHere = AdminRoomOccupant(jid: ada, nick: "Ada", role: .moderator, affiliation: .owner)
        let boHere = AdminRoomOccupant(jid: bo, nick: "bo", role: .participant, affiliation: .none)
        port.admin.occupants[.init(prefix: "lounge")] = .success(AdminPage(rows: [adaHere], nextCursor: "next"))
        port.admin.occupants[.init(prefix: "lounge", cursor: "next")] = .success(AdminPage(rows: [boHere], nextCursor: nil))
        let list = coordinator.adminRoomOccupantsList(of: lounge)

        await list.load()
        await list.loadMore()

        #expect(list.rows == [adaHere, boHere])
    }

    /// The same person from two devices is two rows.
    @Test func someoneInARoomTwiceIsListedUnderEachName() {
        let phone = AdminRoomOccupant(jid: ada, nick: "Ada", role: .participant, affiliation: .member)
        let laptop = AdminRoomOccupant(jid: ada, nick: "Ada (laptop)", role: .participant, affiliation: .member)

        #expect(phone.id != laptop.id)
    }

    @Test func removingSomeoneFromARoomAsksTheServer() async throws {
        let (coordinator, port) = online()

        try await coordinator.removeFromAdminRoom(bo, room: lounge)

        #expect(port.admin.roomWrites == [.remove(lounge.jid, bo)])
    }

    @Test func theRolesOfARoomLoad() async {
        let (coordinator, port) = online()
        let owner = AdminRoomMember(jid: ada, affiliation: .owner, reason: nil)
        let banned = AdminRoomMember(jid: bo, affiliation: .outcast, reason: "Spam")
        port.admin.roomMembers[.init(prefix: "lounge")] = .success(AdminPage(rows: [owner, banned], nextCursor: nil))
        let list = coordinator.adminRoomMembersList(of: lounge)

        await list.load()

        #expect(list.rows == [owner, banned])
    }

    /// The answer names the role the server gave, which is what is shown.
    @Test func changingARoleAnswersWithTheRoleGiven() async throws {
        let (coordinator, port) = online()

        let changed = try await coordinator.setAdminRoomAffiliation(.outcast, of: bo, in: lounge)

        #expect(port.admin.roomWrites == [.setAffiliation(lounge.jid, bo, .outcast)])
        #expect(changed == AdminRoomMember(jid: bo, affiliation: .outcast, reason: nil))
    }
}
