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
