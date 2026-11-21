import SwiftUI
import XCTest
@testable import Waddle
@testable import WaddleKit

/// Night and Daylight goldens for the status picker in the profile. Forms
/// only draw in a real window, so these are screen snapshots (see
/// `assertScreenSnapshot`).
///
/// After an intended visual change, delete the affected goldens under
/// `__Snapshots__/ProfileStatusSnapshotTests` and run the tests once to
/// record new ones (that run fails), then commit them.
@MainActor
final class ProfileStatusSnapshotTests: XCTestCase {
    #if os(iOS)
    private static let size = CGSize(width: 393, height: 460)
    #else
    private static let size = CGSize(width: 480, height: 400)
    #endif

    /// Automatic on a device left alone: its dot is the Away it shows.
    func testAutomaticOnAnIdleDevice() async throws {
        try await assertStatus(mode: .automatic, shown: .away)
    }

    func testDoNotDisturbPickedForEveryDevice() async throws {
        try await assertStatus(mode: .manual(.doNotDisturb), shown: .doNotDisturb)
    }

    private func assertStatus(
        mode: PresenceMode, shown: Availability,
        testName: String = #function, line: UInt = #line
    ) async throws {
        let sample = SampleSession()
        sample.coordinator.status.presenceMode = mode
        sample.coordinator.status.availability = shown
        let view = Form { ProfileStatusSection() }
            .formStyle(.grouped)
        try await assertScreenSnapshot(of: sample.host(view), size: Self.size, testName: testName, line: line)
    }
}
