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
    #else
    func testSplitRoom() async throws {
        let sample = SampleSession()
        sample.seedGeneral()
        sample.navigation.selection = .room(SampleSession.general)
        try await assertScreenSnapshot(of: sample.host(SplitShell()), size: .desktop)
    }
    #endif
}
