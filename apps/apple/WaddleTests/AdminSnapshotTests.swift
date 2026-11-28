import SwiftUI
import XCTest
@testable import Waddle
@testable import WaddleKit

/// Night and Daylight goldens for the admin console (see
/// `assertScreenSnapshot`).
///
/// After an intended visual change, delete the affected goldens under
/// `__Snapshots__/AdminSnapshotTests` and run the tests once to record
/// new ones (that run fails), then commit them.
@MainActor
final class AdminSnapshotTests: XCTestCase {
    #if os(iOS)
    private static let size = CGSize(width: 393, height: 600)
    private static let sheetSize = CGSize.phone
    #else
    private static let size = CGSize(width: 480, height: 600)
    private static let sheetSize = size
    #endif

    /// An owner, three members (one without a display name) and more to load.
    func testUsers() async throws {
        let sample = SampleSession()
        sample.port.admin.users[.init()] = .success(AdminPage(
            rows: [
                AdminUser(jid: SampleSession.ana.jid, displayName: "Ana Lindqvist", isOwner: true),
                AdminUser(jid: SampleSession.jo, displayName: "Jo Marchetti", isOwner: false),
                AdminUser(jid: SampleSession.mika, displayName: nil, isOwner: false),
                AdminUser(jid: SampleSession.priya, displayName: "Priya Raman", isOwner: false),
            ],
            nextCursor: "after-priya"
        ))
        let view = NavigationStack { AdminView() }
        try await assertScreenSnapshot(of: sample.host(view), size: Self.size)
    }

    /// Someone who is not an owner and got here anyway.
    func testAdminAccessOnly() async throws {
        let sample = SampleSession()
        sample.port.admin.users[.init()] = .failure(.rejected)
        let view = NavigationStack { AdminView() }
        try await assertScreenSnapshot(of: sample.host(view), size: Self.size)
    }

    private static let design = AdminSpace(
        service: bare("spaces.waddle.test"), node: "design", name: "Design",
        description: "Pixels, type and critique", roomCount: 3, memberCount: 12
    )

    /// A space with a description, one without, and one room and member.
    func testSpaces() async throws {
        let sample = SampleSession()
        sample.port.admin.spaces[.init()] = .success(AdminPage(
            rows: [
                Self.design,
                AdminSpace(
                    service: bare("spaces.waddle.test"), node: "ops", name: "Ops",
                    description: nil, roomCount: 1, memberCount: 1
                ),
            ],
            nextCursor: nil
        ))
        let view = NavigationStack { AdminView(section: .spaces) }
        try await assertScreenSnapshot(of: sample.host(view), size: Self.size)
    }

    func testNoSpacesYet() async throws {
        let sample = SampleSession()
        let view = NavigationStack { AdminView(section: .spaces) }
        try await assertScreenSnapshot(of: sample.host(view), size: Self.size)
    }

    /// An open space: its members and their roles, and deleting it.
    func testSpace() async throws {
        let sample = SampleSession()
        sample.port.admin.members[.init(prefix: "design")] = .success(AdminPage(
            rows: [
                AdminSpaceMember(jid: SampleSession.ana.jid, role: .owner),
                AdminSpaceMember(jid: SampleSession.jo, role: .admin),
                AdminSpaceMember(jid: SampleSession.mika, role: .member),
            ],
            nextCursor: "after-mika"
        ))
        let sheet = AdminSpaceSheet(subject: .existing(Self.design), spaces: sample.coordinator.adminSpacesList())
        try await assertScreenSnapshot(of: sample.host(sheet), size: Self.sheetSize, asSheet: true)
    }

    /// Adding a member: the role to give, and everyone with an account.
    /// Members of the space show their role and cannot be picked.
    func testAddMember() async throws {
        let sample = SampleSession()
        sample.port.admin.users[.init()] = .success(AdminPage(
            rows: [
                AdminUser(jid: SampleSession.ana.jid, displayName: "Ana Lindqvist", isOwner: true),
                AdminUser(jid: SampleSession.mika, displayName: nil, isOwner: false),
                AdminUser(jid: SampleSession.priya, displayName: "Priya Raman", isOwner: false),
            ],
            nextCursor: "after-priya"
        ))
        sample.port.admin.members[.init(prefix: "design")] = .success(AdminPage(
            rows: [
                AdminSpaceMember(jid: SampleSession.ana.jid, role: .owner),
                AdminSpaceMember(jid: SampleSession.mika, role: .member),
            ],
            nextCursor: nil
        ))
        let picker = AdminPersonPicker(
            title: "Add member", confirmTitle: "Add", roles: [AdminSpaceRole.owner, .admin, .member],
            role: .member, roleTitle: AdminSpaceCopy.title,
            holders: sample.coordinator.adminSpaceMembersList(of: Self.design), roleOf: \.role
        ) { _, _ in }
        try await assertScreenSnapshot(of: sample.host(picker), size: Self.sheetSize, asSheet: true)
    }

    func testNewSpace() async throws {
        let sample = SampleSession()
        let sheet = AdminSpaceSheet(subject: .new, spaces: sample.coordinator.adminSpacesList())
        try await assertScreenSnapshot(of: sample.host(sheet), size: Self.sheetSize, asSheet: true)
    }

    private static let lounge = AdminRoom(
        jid: bare("lounge@rooms.waddle.test"), name: "Lounge", description: "Small talk and coffee",
        kind: .text, access: AdminRoomAccess(isListed: true, isMembersOnly: false), presentCount: 4, memberCount: 9
    )

    /// A plain room, a hidden announcements room and a forum, with every
    /// space to narrow them to.
    func testRooms() async throws {
        let sample = SampleSession()
        sample.port.admin.spaces[.init()] = .success(AdminPage(rows: [Self.design], nextCursor: nil))
        sample.port.admin.rooms[.init()] = .success(AdminPage(
            rows: [
                Self.lounge,
                AdminRoom(
                    jid: bare("board@rooms.waddle.test"), name: "Board", description: nil,
                    kind: .announcement, access: AdminRoomAccess(isListed: false, isMembersOnly: true), presentCount: 0, memberCount: 1
                ),
                AdminRoom(
                    jid: bare("ideas@rooms.waddle.test"), name: "Ideas", description: "Pitch something",
                    kind: .forum, access: AdminRoomAccess(isListed: true, isMembersOnly: false), presentCount: 2, memberCount: 31
                ),
            ],
            nextCursor: nil
        ))
        let view = NavigationStack { AdminView(section: .rooms) }
        try await assertScreenSnapshot(of: sample.host(view), size: Self.size)
    }

    /// Rooms narrowed to one space, with the space chosen beside the title.
    func testRoomsInSpace() async throws {
        let sample = SampleSession()
        sample.port.admin.spaces[.init()] = .success(AdminPage(rows: [Self.design], nextCursor: nil))
        sample.port.admin.rooms[.init(space: "design")] = .success(AdminPage(rows: [Self.lounge], nextCursor: nil))
        let console = AdminConsole(session: sample.coordinator)
        console.roomsSpace = Self.design
        let view = NavigationStack { AdminView(section: .rooms, console: console) }
        try await assertScreenSnapshot(of: sample.host(view), size: Self.size)
    }

    /// An open room: its settings, who is there (one name too long for the
    /// controls beside it), roles, and deleting it.
    func testRoom() async throws {
        let sample = SampleSession()
        sample.port.admin.occupants[.init(prefix: "lounge")] = .success(AdminPage(
            rows: [
                AdminRoomOccupant(jid: SampleSession.ana.jid, nick: "Ana Lindqvist", role: .moderator, affiliation: .owner),
                AdminRoomOccupant(jid: SampleSession.mika, nick: "mika", role: .participant, affiliation: .none),
                // Too long to sit beside the controls: they go under it.
                AdminRoomOccupant(
                    jid: bare("bartholomew.featherstonehaugh@waddle.test"), nick: "Bartholomew Featherstonehaugh-Wright",
                    role: .participant, affiliation: .member
                ),
            ],
            nextCursor: nil
        ))
        sample.port.admin.roomMembers[.init(prefix: "lounge")] = .success(AdminPage(
            rows: [
                AdminRoomMember(jid: SampleSession.ana.jid, affiliation: .owner, reason: nil),
                AdminRoomMember(jid: SampleSession.jo, affiliation: .member, reason: nil),
                AdminRoomMember(jid: SampleSession.theo, affiliation: .outcast, reason: "Spam"),
            ],
            nextCursor: nil
        ))
        let sheet = AdminRoomSheet(subject: .existing(Self.lounge), rooms: sample.coordinator.adminRoomsList())
        try await assertScreenSnapshot(
            of: sample.host(sheet), size: CGSize(width: Self.sheetSize.width, height: 1000), asSheet: true
        )
    }

    /// A new room, with the spaces it can go in.
    func testNewRoom() async throws {
        let sample = SampleSession()
        sample.port.admin.spaces[.init()] = .success(AdminPage(rows: [Self.design], nextCursor: nil))
        let sheet = AdminRoomSheet(subject: .new, rooms: sample.coordinator.adminRoomsList())
        try await assertScreenSnapshot(of: sample.host(sheet), size: Self.sheetSize, asSheet: true)
    }

    #if os(macOS)
    /// The sidebar's Admin row, which only community owners see.
    func testSplitAdmin() async throws {
        let sample = SampleSession()
        sample.coordinator.isCommunityOwner = true
        sample.port.admin.users[.init()] = .success(AdminPage(
            rows: [AdminUser(jid: SampleSession.ana.jid, displayName: "Ana Lindqvist", isOwner: true)],
            nextCursor: nil
        ))
        sample.navigation.show(.admin)
        try await assertScreenSnapshot(of: sample.host(SplitShell()), size: .desktop)
    }
    #endif
}
