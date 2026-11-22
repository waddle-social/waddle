import SwiftUI
import XCTest
@testable import Waddle

/// Night and Daylight goldens for a conversation: the room on the phone
/// stack on iOS, in the split view on macOS. Compared on the OS the
/// goldens were recorded on; see `assertScreenSnapshot`.
@MainActor
final class ConversationScreenSnapshotTests: XCTestCase {
    #if os(iOS)
    func testPhoneRoom() async throws {
        let sample = SampleSession()
        sample.seedGeneral()
        sample.navigation.homePath = [.conversation(.room(SampleSession.general))]
        try await assertScreenSnapshot(of: sample.host(PhoneShell()), size: .phone)
    }

    func testPhoneDirect() async throws {
        let sample = SampleSession()
        sample.seedTheo()
        sample.navigation.tab = .directMessages
        sample.navigation.directPath = [.conversation(.direct(SampleSession.theo))]
        try await assertScreenSnapshot(of: sample.host(PhoneShell()), size: .phone)
    }
    #else
    func testSplitRoom() async throws {
        let sample = SampleSession()
        sample.seedGeneral()
        sample.navigation.selection = .room(SampleSession.general)
        try await assertScreenSnapshot(of: sample.host(SplitShell()), size: .desktop)
    }

    /// The timeline fills the pane and starts at its leading edge, as on
    /// the web, rather than sitting in a centred column.
    func testSplitRoomWide() async throws {
        let sample = SampleSession()
        sample.seedGeneral()
        sample.navigation.selection = .room(SampleSession.general)
        try await assertScreenSnapshot(of: sample.host(SplitShell()), size: .wideDesktop)
    }

    func testSplitDirect() async throws {
        let sample = SampleSession()
        sample.seedTheo()
        sample.navigation.selection = .direct(SampleSession.theo)
        try await assertScreenSnapshot(of: sample.host(SplitShell()), size: .desktop)
    }
    #endif
}
