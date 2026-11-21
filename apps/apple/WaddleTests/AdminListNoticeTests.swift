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
    func testOnlyForbiddenMeansNotAnOwner() {
        XCTAssertEqual(FFIXmppPort.adminError(WaddleError.Stanza(condition: "forbidden", text: nil)), .rejected)
        XCTAssertEqual(FFIXmppPort.adminError(WaddleError.Stanza(condition: "internal-server-error", text: nil)), .failed)
        XCTAssertEqual(FFIXmppPort.adminError(WaddleError.Stanza(condition: "bad-request", text: nil)), .failed)
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

    func testOtherErrorsKeepTheirMeaning() {
        XCTAssertEqual(FFIXmppPort.adminError(WaddleError.NotConnected), .notConnected)
        XCTAssertEqual(FFIXmppPort.adminError(WaddleError.Timeout), .timeout)
    }
}

/// How the console words a space.
final class AdminSpaceCopyTests: XCTestCase {
    func testCountsSayOneOrMany() {
        XCTAssertEqual(AdminSpaceCopy.counts(rooms: 1, members: 1), "1 room · 1 member")
        XCTAssertEqual(AdminSpaceCopy.counts(rooms: 3, members: 12), "3 rooms · 12 members")
        XCTAssertEqual(AdminSpaceCopy.counts(rooms: 0, members: 0), "0 rooms · 0 members")
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
            isListed: isListed, isMembersOnly: isMembersOnly, presentCount: 0, memberCount: 0
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

    func testRolesAreNamedWithoutProtocolWords() {
        XCTAssertEqual(AdminRoomCopy.title(RoomAffiliation.outcast), "Banned")
        XCTAssertEqual(AdminRoomCopy.title(RoomAffiliation.none), "No role")
        XCTAssertEqual(AdminRoomCopy.title(AdminRoomKind.other), "Other")
    }
}
