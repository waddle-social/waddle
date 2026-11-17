import SwiftUI
import XCTest
@testable import Waddle

/// Night and Daylight goldens for the signed-in shell: the phone tabs on
/// iOS, the split view on macOS. Compared on the OS the goldens were
/// recorded on; see `assertScreenSnapshot`.
@MainActor
final class ShellScreenSnapshotTests: XCTestCase {
    #if os(iOS)
    func testPhoneHome() async throws {
        let sample = SampleSession()
        try await assertScreenSnapshot(of: sample.host(PhoneShell()), size: .phone)
    }

    func testPhoneDirectMessages() async throws {
        let sample = SampleSession()
        sample.navigation.tab = .directMessages
        try await assertScreenSnapshot(of: sample.host(PhoneShell()), size: .phone)
    }

    func testPhoneInbox() async throws {
        let sample = SampleSession()
        sample.seedInbox()
        sample.navigation.tab = .inbox
        try await assertScreenSnapshot(of: sample.host(PhoneShell()), size: .phone)
    }
    #else
    func testSplitHome() async throws {
        let sample = SampleSession()
        try await assertScreenSnapshot(of: sample.host(SplitShell()), size: .desktop)
    }

    func testSplitInbox() async throws {
        let sample = SampleSession()
        sample.seedInbox()
        sample.navigation.show(.inbox)
        try await assertScreenSnapshot(of: sample.host(SplitShell()), size: .desktop)
    }
    #endif
}
