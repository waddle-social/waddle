import WaddleKit
import XCTest
@testable import Waddle

/// What a list of the admin console says in place of its rows.
final class AdminListNoticeTests: XCTestCase {
    func testRowsAreShownWithoutANotice() {
        XCTAssertNil(AdminListNotice(isEmpty: false, hasLoaded: true, failure: nil, query: ""))
        XCTAssertNil(AdminListNotice(isEmpty: false, hasLoaded: true, failure: .timeout, query: ""))
    }

    /// "No one" is only said once the server has said so.
    func testAListNotLoadedYetIsLoadingNotEmpty() {
        XCTAssertEqual(AdminListNotice(isEmpty: true, hasLoaded: false, failure: nil, query: ""), .loading)
        XCTAssertEqual(AdminListNotice(isEmpty: true, hasLoaded: false, failure: nil, query: "ada"), .loading)
    }

    func testAnEmptyAnswerIsNoOneOrNoMatches() {
        XCTAssertEqual(AdminListNotice(isEmpty: true, hasLoaded: true, failure: nil, query: " "), .empty)
        XCTAssertEqual(AdminListNotice(isEmpty: true, hasLoaded: true, failure: nil, query: "ada"), .noMatches)
    }

    func testSomeoneWhoIsNotAnOwnerIsToldSo() {
        XCTAssertEqual(AdminListNotice(isEmpty: true, hasLoaded: false, failure: .rejected, query: ""), .notAllowed)
    }

    func testAFailureSaysWhetherItIsTheConnection() {
        XCTAssertEqual(AdminListNotice(isEmpty: true, hasLoaded: false, failure: .notConnected, query: ""), .offline)
        XCTAssertEqual(AdminListNotice(isEmpty: true, hasLoaded: false, failure: .timeout, query: ""), .failed)
        XCTAssertEqual(AdminListNotice(isEmpty: true, hasLoaded: true, failure: .failed, query: "ada"), .failed)
    }
}

/// What crosses the bridge for the admin console, and which of the
/// server's answers mean "not an owner".
final class AdminErrorTests: XCTestCase {
    private func listError(_ error: Error) -> PortError? {
        FFIXmppPort.adminError(error) as? PortError
    }

    func testOnlyForbiddenMeansNotAnOwner() {
        XCTAssertEqual(listError(WaddleError.Stanza(condition: "forbidden", text: nil)), .rejected)
        XCTAssertEqual(listError(WaddleError.Stanza(condition: "internal-server-error", text: nil)), .failed)
        XCTAssertEqual(listError(WaddleError.Stanza(condition: "bad-request", text: nil)), .failed)
        XCTAssertEqual(listError(WaddleError.Stanza(condition: "not-allowed", text: nil)), .failed)
    }

    /// The server sends an empty string for a description a space lacks.
    func testAnEmptyDescriptionIsNoDescription() {
        XCTAssertNil(FFIXmppPort.nonEmpty(""))
        XCTAssertNil(FFIXmppPort.nonEmpty(nil))
        XCTAssertEqual(FFIXmppPort.nonEmpty("Pixels"), "Pixels")
    }

    func testRolesCrossTheBridgeUnchanged() {
        for role in AdminSpaceRole.allCases {
            XCTAssertEqual(FFIXmppPort.role(FFIXmppPort.role(role)), role)
        }
        XCTAssertEqual(FFIXmppPort.role(AdminSpaceRole.none), WaddleSpaceRole.none)
        XCTAssertEqual(FFIXmppPort.role(AdminSpaceRole.owner), WaddleSpaceRole.owner)
    }

    /// The server names someone in a room by the device they are there
    /// from; the console lists the person.
    func testSomeoneInARoomIsNamedWithoutTheirDevice() {
        XCTAssertEqual(FFIXmppPort.person("ana@waddle.test/phone"), bare("ana@waddle.test"))
        XCTAssertEqual(FFIXmppPort.person("ana@waddle.test"), bare("ana@waddle.test"))
        XCTAssertNil(FFIXmppPort.person("not an address"))
    }

    /// A kind the console does not make is shown and never sent.
    func testRoomKindsCrossTheBridge() {
        XCTAssertEqual(FFIXmppPort.roomKind("text"), .text)
        XCTAssertEqual(FFIXmppPort.roomKind("announcement"), .announcement)
        XCTAssertEqual(FFIXmppPort.roomKind("forum"), .forum)
        XCTAssertEqual(FFIXmppPort.roomKind("stage"), .other)
        XCTAssertEqual(FFIXmppPort.wire(.forum), "forum")
        XCTAssertNil(FFIXmppPort.wire(.other))
    }

    func testAConfirmationSaysWhatCannotBePutBack() {
        let member = AdminRoomMember(jid: bare("jo@waddle.test"), affiliation: .member, reason: nil)
        XCTAssertEqual(
            AdminRoomSheet.PendingAction.ban(member).message(roomName: "Lounge"),
            "jo@waddle.test is taken out of Lounge and can't join it until the ban is changed."
        )
        XCTAssertEqual(AdminRoomSheet.PendingAction.ban(member).confirmTitle, "Ban")
        XCTAssertEqual(AdminRoomSheet.PendingAction.removeRole(member).title, "Remove role?")
    }

    /// A role can be given again from the console, so neither removal
    /// says it cannot.
    func testARemovalSaysWhatItEnds() {
        let member = AdminRoomMember(jid: bare("jo@waddle.test"), affiliation: .member, reason: nil)
        let occupant = AdminRoomOccupant(jid: bare("jo@waddle.test"), nick: "jo", role: .participant, affiliation: .member)
        XCTAssertEqual(
            AdminRoomSheet.PendingAction.removeRole(member).message(roomName: "Lounge"),
            "jo@waddle.test will have no role in Lounge."
        )
        XCTAssertEqual(
            AdminRoomSheet.PendingAction.removeMember(occupant).message(roomName: "Lounge"),
            "Lounge is for members only, so jo also stops being a member and can't come back until they're made one again."
        )
    }

    /// A role change keeps every refusal `rejected` but the two the
    /// server explains.
    func testARefusedRoleSaysWhyOnlyWhereTheServerDoes() {
        func roleError(_ condition: String) -> Error {
            FFIXmppPort.adminError(WaddleError.Stanza(condition: condition, text: nil), changingRole: true)
        }
        XCTAssertEqual(roleError("not-allowed") as? AdminRoleRefusal, .notAllowed)
        XCTAssertEqual(roleError("conflict") as? AdminRoleRefusal, .lastOwner)
        XCTAssertEqual(roleError("forbidden") as? PortError, .rejected)
        XCTAssertEqual(roleError("item-not-found") as? PortError, .rejected)
        XCTAssertEqual(FFIXmppPort.adminError(WaddleError.Timeout, changingRole: true) as? PortError, .timeout)
    }

    func testOtherErrorsKeepTheirMeaning() {
        XCTAssertEqual(listError(WaddleError.NotConnected), .notConnected)
        XCTAssertEqual(listError(WaddleError.Timeout), .timeout)
    }
}

/// How the console words a space.
final class AdminSpaceCopyTests: XCTestCase {
    func testCountsSayOneOrMany() {
        XCTAssertEqual(AdminSpaceCopy.counts(rooms: 1, members: 1), "1 room · 1 member")
        XCTAssertEqual(AdminSpaceCopy.counts(rooms: 3, members: 12), "3 rooms · 12 members")
        XCTAssertEqual(AdminSpaceCopy.counts(rooms: 0, members: 0), "0 rooms · 0 members")
    }

    func testARefusedRoleSaysEveryReasonTheServerHas() {
        XCTAssertEqual(
            AdminSpaceCopy.roleFailure(AdminRoleRefusal.notAllowed, address: "cy@elsewhere.test"),
            "The server doesn't allow this. Only people with an account here or on a trusted server can be an owner or admin, and a space that belongs to another community is managed there."
        )
        XCTAssertEqual(
            AdminSpaceCopy.roleFailure(PortError.rejected, address: "cy@elsewhere.test"),
            "The server declined this request."
        )
        XCTAssertEqual(
            AdminSpaceCopy.roleFailure(PortError.failed, address: "cy@elsewhere.test"),
            "Couldn't change the role of cy@elsewhere.test. Try again."
        )
    }

    func testTheDeleteWarningSaysRoomsGoToo() {
        XCTAssertEqual(AdminSpaceCopy.deleteWarning(name: "Ops", rooms: 0), "This deletes Ops. This can't be undone.")
        XCTAssertEqual(
            AdminSpaceCopy.deleteWarning(name: "Ops", rooms: 3),
            "This deletes Ops and the rooms that belong to it. This can't be undone."
        )
    }
}

/// How the console words a room.
final class AdminRoomCopyTests: XCTestCase {
    private func room(kind: AdminRoomKind, isListed: Bool, isMembersOnly: Bool) -> AdminRoom {
        AdminRoom(
            jid: bare("lounge@rooms.waddle.test"), name: "Lounge", description: nil, kind: kind,
            access: AdminRoomAccess(isListed: isListed, isMembersOnly: isMembersOnly), presentCount: 0, memberCount: 0
        )
    }

    func testCountsSayOneOrManyMembers() {
        XCTAssertEqual(AdminRoomCopy.counts(present: 4, members: 9), "4 here · 9 members")
        XCTAssertEqual(AdminRoomCopy.counts(present: 0, members: 1), "0 here · 1 member")
    }

    /// A text room anyone can find and join needs no tags.
    func testOnlyWhatIsUnusualAboutARoomIsTagged() {
        XCTAssertEqual(AdminRoomCopy.tags(for: room(kind: .text, isListed: true, isMembersOnly: false)), [])
        XCTAssertEqual(
            AdminRoomCopy.tags(for: room(kind: .announcement, isListed: false, isMembersOnly: true)),
            ["Announcements", "Hidden", "Members only"]
        )
        XCTAssertEqual(
            AdminRoomCopy.tags(for: room(kind: .other, isListed: true, isMembersOnly: true)),
            ["Other", "Members only"]
        )
    }

    func testNoMatchesNamesTheTextAndTheSpace() {
        let noMatches = AdminListCopy.rooms.noMatches
        XCTAssertEqual(noMatches("lo", nil), "No room's name starts with “lo”.")
        XCTAssertEqual(noMatches("lo", "Design"), "No room in Design has a name that starts with “lo”.")
        XCTAssertEqual(
            AdminListCopy.rooms.emptyInSpace?("Design"), "Design has no rooms yet. Make one to get people talking."
        )
    }

    func testARefusedRoleSaysWhy() {
        XCTAssertEqual(
            AdminRoomCopy.roleFailure(AdminRoleRefusal.lastOwner, address: "ana@waddle.test"),
            "ana@waddle.test is the room's last owner. Make someone else an owner first."
        )
        XCTAssertEqual(
            AdminRoomCopy.roleFailure(AdminRoleRefusal.notAllowed, address: "cy@elsewhere.test"),
            "The server doesn't allow this. Only people with an account here or on a trusted server can be an owner or admin, and not while they're in the room over a connection the server hasn't verified. A room that belongs to another community is managed there."
        )
        XCTAssertEqual(
            AdminRoomCopy.roleFailure(PortError.timeout, address: "cy@elsewhere.test"),
            "The server took too long to answer. Try again."
        )
    }

    func testRolesAreNamedWithoutProtocolWords() {
        XCTAssertEqual(AdminRoomCopy.title(RoomAffiliation.outcast), "Banned")
        XCTAssertEqual(AdminRoomCopy.title(RoomAffiliation.none), "No role")
        XCTAssertEqual(AdminRoomCopy.title(AdminRoomKind.other), "Other")
    }
}

/// The space the console's rooms are narrowed to.
@MainActor
final class AdminConsoleTests: XCTestCase {
    private let design = AdminSpace(
        service: bare("spaces.waddle.test"), node: "design", name: "Design",
        description: nil, roomCount: 1, memberCount: 2
    )

    /// The console's lists hold the session weakly.
    private var sample: SampleSession?

    private func console(spaces: [AdminSpace]) -> AdminConsole {
        let sample = SampleSession()
        self.sample = sample
        sample.port.admin.spaces[.init()] = .success(AdminPage(rows: spaces, nextCursor: nil))
        return AdminConsole(session: sample.coordinator)
    }

    /// Before the spaces load, the one chosen is still offered.
    func testTheChosenSpaceIsOfferedWhileTheSpacesLoad() {
        let console = console(spaces: [])
        console.roomsSpace = design

        XCTAssertEqual(console.spaceFilterChoices, [design])
    }

    func testAChosenSpaceThatIsGoneStopsNarrowingTheRooms() async {
        let ops = AdminSpace(service: design.service, node: "ops", name: "Ops", description: nil, roomCount: 0, memberCount: 0)
        let console = console(spaces: [ops])
        console.roomsSpace = design

        await console.refreshSpaceChoices()

        XCTAssertNil(console.roomsSpace)
        XCTAssertEqual(console.spaceFilterChoices, [ops])
    }

    /// A space renamed since it was chosen is shown by its new name.
    func testTheChosenSpaceFollowsItsRow() async {
        let studio = AdminSpace(
            service: design.service, node: "design", name: "Studio", description: nil, roomCount: 1, memberCount: 2
        )
        let console = console(spaces: [studio])
        console.roomsSpace = design

        await console.refreshSpaceChoices()

        XCTAssertEqual(console.roomsSpace, studio)
    }
}
