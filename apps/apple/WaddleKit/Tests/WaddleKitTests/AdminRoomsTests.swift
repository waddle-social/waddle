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
        kind: .text, access: AdminRoomAccess(isListed: true, isMembersOnly: false), presentCount: 4, memberCount: 9
    )
    private let board = AdminRoom(
        jid: bare("board@muc.waddle.test"), name: "Board", description: nil,
        kind: .announcement, access: AdminRoomAccess(isListed: false, isMembersOnly: true), presentCount: 0, memberCount: 3
    )
    private let ada = bare("ada@waddle.test")
    private let bo = bare("bo@waddle.test")

    private func draft(
        _ name: String = "Lounge", description: String = "", kind: AdminRoomKind = .text,
        isListed: Bool = true, isMembersOnly: Bool = false, space: AdminSpace? = nil
    ) throws -> AdminRoomDraft {
        try #require(AdminRoomDraft(
            name: name, description: description, kind: kind,
            access: AdminRoomAccess(isListed: isListed, isMembersOnly: isMembersOnly), space: space
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
        #expect(AdminRoomDraft(name: " ", description: "", kind: .text, access: AdminRoomAccess(isListed: true, isMembersOnly: false), space: nil) == nil)
        #expect(AdminRoomDraft(
            name: String(repeating: "a", count: 81), description: "", kind: .text,
            access: AdminRoomAccess(isListed: true, isMembersOnly: false), space: nil
        ) == nil)
    }

    /// The server has rooms that are hidden and open to anyone with the
    /// address; a draft says what it is given and decides nothing.
    @Test func aDraftKeepsTheAccessItIsGiven() throws {
        #expect(try !draft(isListed: false, isMembersOnly: false).access.isMembersOnly)
        #expect(try draft(isListed: true, isMembersOnly: true).access.isMembersOnly)
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

    // MARK: Narrowed to a space

    private let design = AdminSpace(
        service: bare("spaces.waddle.test"), node: "design", name: "Design",
        description: nil, roomCount: 1, memberCount: 2
    )

    @Test func theRoomsOfASpaceAreAskedForByTheSpace() async {
        let (coordinator, port) = online()
        port.admin.rooms[.init(space: "design")] = .success(AdminPage(rows: [lounge], nextCursor: "next"))
        port.admin.rooms[.init(cursor: "next", space: "design")] = .success(AdminPage(rows: [board], nextCursor: nil))
        let list = coordinator.adminRoomsList()

        await list.load(in: design)
        await list.loadMore()
        await list.reload()

        #expect(list.rows == [lounge])
        #expect(list.space == design)
        #expect(port.admin.roomsRequests == [
            .init(space: "design"), .init(cursor: "next", space: "design"), .init(space: "design"),
        ])
    }

    /// Choosing another space while one is on its way shows the one
    /// chosen last.
    @Test func aSlowerAnswerForTheSpaceBeforeIsDropped() async {
        let (coordinator, port) = online()
        port.admin.rooms[.init(space: "design")] = .success(AdminPage(rows: [lounge], nextCursor: nil))
        port.admin.rooms[.init()] = .success(AdminPage(rows: [board], nextCursor: nil))
        let list = coordinator.adminRoomsList()
        port.admin.roomsFetches.isHolding = true
        let first = Task { await list.load(in: design) }
        await eventually { port.admin.roomsFetches.count == 1 }
        port.admin.roomsFetches.isHolding = false

        await list.load()
        port.admin.roomsFetches.release()
        await first.value

        #expect(list.rows == [board])
        #expect(list.space == nil)
        #expect(!list.isLoading)
    }

    /// "All spaces" chosen again before a space answers: every room stays
    /// shown, and a refresh asks for every room, not the space.
    @Test func aRefreshAfterACancelledSpaceAsksForWhatIsShown() async {
        let (coordinator, port) = online()
        port.admin.rooms[.init()] = .success(AdminPage(rows: [lounge, board], nextCursor: nil))
        port.admin.rooms[.init(space: "design")] = .success(AdminPage(rows: [lounge], nextCursor: nil))
        let list = coordinator.adminRoomsList()
        await list.load()
        port.admin.roomsFetches.isHolding = true
        let narrowing = Task { await list.load(in: design) }
        await eventually { port.admin.roomsFetches.count == 1 }

        list.want("", in: nil)
        narrowing.cancel()
        port.admin.roomsFetches.isHolding = false
        port.admin.roomsFetches.release()
        await narrowing.value
        await list.reload()

        #expect(port.admin.roomsRequests.last == .init())
        #expect(list.rows == [lounge, board])
        #expect(list.space == nil)
    }

    /// Back to space A before B answers: a refresh in that window asks for
    /// A, and B's rooms are not shown over A's.
    @Test func aRefreshBeforeACancelledSpaceAnswersAsksForTheSpaceWanted() async {
        let (coordinator, port) = online()
        let ops = AdminSpace(service: design.service, node: "ops", name: "Ops", description: nil, roomCount: 1, memberCount: 0)
        port.admin.rooms[.init(space: "design")] = .success(AdminPage(rows: [lounge], nextCursor: nil))
        port.admin.rooms[.init(space: "ops")] = .success(AdminPage(rows: [board], nextCursor: nil))
        let list = coordinator.adminRoomsList()
        await list.load(in: design)
        port.admin.roomsFetches.isHolding = true
        let narrowing = Task { await list.load(in: ops) }
        await eventually { port.admin.roomsFetches.count == 1 }
        port.admin.roomsFetches.isHolding = false

        list.want("", in: design)
        narrowing.cancel()
        await list.reload()
        port.admin.roomsFetches.release()
        await narrowing.value

        #expect(port.admin.roomsRequests.last == .init(space: "design"))
        #expect(list.rows == [lounge])
        #expect(list.space == design)
    }

    /// Rooms of every space are not an answer for one space.
    @Test func aSpaceThatFailsShowsNoRoomsFromBefore() async {
        let (coordinator, port) = online()
        port.admin.rooms[.init()] = .success(AdminPage(rows: [lounge, board], nextCursor: nil))
        port.admin.rooms[.init(space: "design")] = .failure(.timeout)
        let list = coordinator.adminRoomsList()
        await list.load()

        await list.load(in: design)

        #expect(list.rows.isEmpty)
        #expect(!list.hasLoaded)
        #expect(list.space == design)
        #expect(list.failure == .timeout)
    }

    @Test func aSpaceAndTextAreAskedForTogether() async {
        let (coordinator, port) = online()
        port.admin.rooms[.init(prefix: "l", space: "design")] = .success(AdminPage(rows: [lounge], nextCursor: nil))
        let list = coordinator.adminRoomsList()

        await list.load(matching: "l", in: design)

        #expect(list.rows == [lounge])
        #expect(list.query == "l")
    }

    /// The spaces for a picker: every page, stopping at one that fails.
    @Test func everyPageCanBeLoaded() async {
        let (coordinator, port) = online()
        let ops = AdminSpace(service: design.service, node: "ops", name: "Ops", description: nil, roomCount: 0, memberCount: 0)
        port.admin.spaces[.init()] = .success(AdminPage(rows: [design], nextCursor: "next"))
        port.admin.spaces[.init(cursor: "next")] = .success(AdminPage(rows: [ops], nextCursor: nil))
        let list = coordinator.adminSpacesList()
        #expect(!list.isComplete)

        await list.loadEveryPage()

        #expect(list.rows == [design, ops])
        #expect(list.isComplete)

        port.admin.spaces[.init(cursor: "next")] = .failure(.timeout)
        await list.loadEveryPage()
        #expect(list.rows == [design])
        #expect(!list.isComplete)
        #expect(list.moreFailure == .timeout)
    }

    // MARK: Making, changing, deleting

    @Test func creatingARoomSendsItsDraftAndLooksAtTheDirectoryAgain() async throws {
        let (coordinator, port) = online()
        let draft = try draft()

        try await coordinator.createAdminRoom(draft)

        #expect(port.admin.roomWrites == [.create(draft)])
        #expect(port.topologyRequests == 1)
    }

    @Test func creatingFromTheSidebarResolvesTheSpaceAndOpensTheReturnedRoom() async throws {
        let (coordinator, port) = online()
        coordinator.isCommunityOwner = true
        let space = Space(id: "design/lab", serviceJID: bare("spaces.waddle.test"), name: "Design Lab", summary: nil)
        coordinator.directory.apply(Topology(spaces: [space], channels: []))
        let adminSpace = AdminSpace(
            service: bare(#"design\2flab@spaces.waddle.test"#),
            node: space.id, name: space.name, description: nil, roomCount: 0, memberCount: 0
        )
        port.admin.spaces[.init()] = .success(AdminPage(rows: [], nextCursor: "more"))
        port.admin.spaces[.init(cursor: "more")] = .success(AdminPage(rows: [adminSpace], nextCursor: nil))
        port.topologyFailure = .timeout
        let created = try await coordinator.createChannel(name: "Sketches", summary: "Ideas", in: space)

        #expect(created == .room(port.admin.createdRoom))
        #expect(coordinator.directory.channels(in: space.id).map(\.roomJID) == [port.admin.createdRoom])
        #expect(port.admin.roomWrites == [.create(try draft("Sketches", description: "Ideas", space: adminSpace))])
        #expect(port.topologyRequests == 1)
    }

    @Test func creatingFromTheSidebarRefusesNonOwnersAndAnUnlistedSpace() async throws {
        let (coordinator, port) = online()
        let space = Space(id: "design", serviceJID: bare("spaces.waddle.test"), name: "Design", summary: nil)
        coordinator.directory.apply(Topology(spaces: [space], channels: []))

        await #expect(throws: PortError.rejected) {
            try await coordinator.createChannel(name: "Sketches", summary: nil, in: space)
        }
        coordinator.isCommunityOwner = true
        await #expect(throws: PortError.rejected) {
            try await coordinator.createChannel(name: "Sketches", summary: nil, in: space)
        }
        #expect(port.admin.roomWrites.isEmpty)
    }

    /// The answer has no counts; they stay as they were.
    @Test func changingARoomAnswersWithWhatTheServerHolds() async throws {
        let (coordinator, port) = online()
        let draft = try draft("Salon", description: "Chat", kind: .forum, isListed: false)

        let changed = try await coordinator.updateAdminRoom(lounge, to: draft)

        #expect(port.admin.roomWrites == [.update(lounge.jid, draft)])
        #expect(changed.name == "Salon")
        #expect(changed.kind == .forum)
        #expect(!changed.access.isListed)
        #expect(!changed.access.isMembersOnly)
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

    // MARK: Reasons

    @Test func aRemovalSendsItsReason() async throws {
        let (coordinator, port) = online()

        try await coordinator.removeFromAdminRoom(bo, room: lounge, reason: " Flooding ")

        #expect(port.admin.roomWrites == [.remove(lounge.jid, bo, reason: "Flooding")])
    }

    /// The reason is sent with a ban. The server keeps none for a role
    /// (#2601), so the role shown after it has none either.
    @Test func aBanSendsItsReason() async throws {
        let (coordinator, port) = online()

        let banned = try await coordinator.setAdminRoomAffiliation(.outcast, of: bo, in: lounge, reason: "Spam")

        #expect(port.admin.roomWrites == [.setAffiliation(lounge.jid, bo, .outcast, reason: "Spam")])
        #expect(banned == AdminRoomMember(jid: bo, affiliation: .outcast, reason: nil))
    }

    /// The server's reasons for refusing a role reach the console as they
    /// are, so it can say them.
    @Test func aRefusedRoleSaysWhy() async {
        let (coordinator, port) = online()
        port.admin.roleRefusal = .lastOwner

        await #expect(throws: AdminRoleRefusal.lastOwner) {
            try await coordinator.setAdminRoomAffiliation(.member, of: ada, in: lounge)
        }
    }

    @Test func aBlankReasonIsNoReason() async throws {
        let (coordinator, port) = online()

        try await coordinator.removeFromAdminRoom(bo, room: lounge, reason: "  ")
        let banned = try await coordinator.setAdminRoomAffiliation(.outcast, of: bo, in: lounge, reason: "\n")

        #expect(port.admin.roomWrites == [
            .remove(lounge.jid, bo, reason: nil), .setAffiliation(lounge.jid, bo, .outcast, reason: nil),
        ])
        #expect(banned.reason == nil)
    }

    /// Someone who had no role is listed first once given one.
    @Test func someoneGivenARoleIsListedFirst() async throws {
        let (coordinator, port) = online()
        let owner = AdminRoomMember(jid: ada, affiliation: .owner, reason: nil)
        port.admin.roomMembers[.init(prefix: "lounge")] = .success(AdminPage(rows: [owner], nextCursor: nil))
        let list = coordinator.adminRoomMembersList(of: lounge)
        await list.load()

        list.add(try await coordinator.setAdminRoomAffiliation(.member, of: bo, in: lounge))

        #expect(port.admin.roomWrites == [.setAffiliation(lounge.jid, bo, .member)])
        #expect(list.rows == [AdminRoomMember(jid: bo, affiliation: .member, reason: nil), owner])
    }
}
