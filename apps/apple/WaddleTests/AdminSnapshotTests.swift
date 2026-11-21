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
    #else
    private static let size = CGSize(width: 480, height: 600)
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
        address: "spaces.waddle.test", node: "design", name: "Design",
        description: "Pixels, type and critique", roomCount: 3, memberCount: 12
    )

    /// A space with a description, one without, and one room and member.
    func testSpaces() async throws {
        let sample = SampleSession()
        sample.port.admin.spaces[.init()] = .success(AdminPage(
            rows: [
                Self.design,
                AdminSpace(
                    address: "spaces.waddle.test", node: "ops", name: "Ops",
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
        let sheet = AdminSpaceSheet(subject: .space(Self.design), spaces: sample.coordinator.adminSpacesList())
        try await assertScreenSnapshot(of: sample.host(sheet), size: Self.size)
    }

    func testNewSpace() async throws {
        let sample = SampleSession()
        let sheet = AdminSpaceSheet(subject: .new, spaces: sample.coordinator.adminSpacesList())
        try await assertScreenSnapshot(of: sample.host(sheet), size: Self.size)
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
