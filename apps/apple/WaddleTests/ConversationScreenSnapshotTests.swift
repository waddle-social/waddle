import SwiftUI
import XCTest
@testable import Waddle
@testable import WaddleKit

/// Night and Daylight goldens for a conversation: the room on the phone
/// stack on iOS, in the split view on macOS. Compared on the OS the
/// goldens were recorded on; see `assertScreenSnapshot`.
@MainActor
final class ConversationScreenSnapshotTests: XCTestCase {
    func testDirectWithPeerInACall() async throws {
        let sample = SampleSession()
        sample.seedTheo()
        sample.coordinator.handle(.activityChanged(
            jid: SampleSession.theo, activity: UserActivity(general: "talking", specific: "on_video_phone")
        ))
        let direct = ConversationID.direct(SampleSession.theo)
        XCTAssertEqual(ConversationHeaderText.subtitle(for: direct, session: sample.coordinator), "In a call")
        #if os(iOS)
        sample.navigation.tab = .directMessages
        sample.navigation.directPath = [.conversation(direct)]
        try await assertScreenSnapshot(of: sample.host(PhoneShell()), size: .phone)
        #else
        sample.navigation.selection = direct
        try await assertScreenSnapshot(of: sample.host(SplitShell()), size: .desktop)
        #endif
    }

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

    /// The split view builds the conversation screen twice; the second
    /// must still show the "New messages" divider the first open found.
    func testSplitRoomWithUnreadMessages() async throws {
        let sample = SampleSession()
        sample.seedGeneral()
        sample.coordinator.unread.set(2, for: .room(SampleSession.general))
        sample.navigation.selection = .room(SampleSession.general)
        try await assertScreenSnapshot(of: sample.host(SplitShell()), size: .desktop)
        XCTAssertNotNil(sample.coordinator.unreadAnchor(for: .room(SampleSession.general)))
    }

    func testSplitDirect() async throws {
        let sample = SampleSession()
        sample.seedTheo()
        sample.navigation.selection = .direct(SampleSession.theo)
        try await assertScreenSnapshot(of: sample.host(SplitShell()), size: .desktop)
    }
    #endif
}
