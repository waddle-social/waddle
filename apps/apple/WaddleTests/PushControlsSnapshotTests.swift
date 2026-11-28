import SwiftUI
import XCTest
@testable import Waddle
@testable import WaddleKit

/// Night and Daylight goldens for the notification controls: the Notifications
/// settings with notifications on and off, and a conversation whose notifications
/// show messages. Forms only draw in a real window, so these are screen
/// snapshots (see `assertScreenSnapshot`).
///
/// After an intended visual change, delete the affected goldens under
/// `__Snapshots__/PushControlsSnapshotTests` and run the tests once to
/// record new ones (that run fails), then commit them.
@MainActor
final class PushControlsSnapshotTests: XCTestCase {
    #if os(iOS)
    private static let settings = CGSize(width: 393, height: 520)
    private static let details = CGSize(width: 393, height: 760)
    #else
    private static let settings = CGSize(width: 520, height: 420)
    private static let details = CGSize(width: 360, height: 760)
    #endif

    func testNotificationSettingsWithPushOn() async throws {
        try await assertSettings(notificationsEnabled: true)
    }

    /// The registration row goes away: there is nothing to register.
    func testNotificationSettingsWithPushOff() async throws {
        try await assertSettings(notificationsEnabled: false)
    }

    func testDetailsOfAConversationShowingMessages() async throws {
        let sample = SampleSession()
        sample.coordinator.directory.setShowsMessagesInPush(true, for: .direct(SampleSession.priya))
        let view = NavigationStack {
            ConversationDetailsView(conversation: .direct(SampleSession.priya))
        }
        try await assertScreenSnapshot(of: sample.host(view), size: Self.details)
    }

    private func assertSettings(
        notificationsEnabled: Bool, testName: String = #function, line: UInt = #line
    ) async throws {
        let suite = "push-controls-snapshots"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defaults.removePersistentDomain(forName: suite)
        defer { defaults.removePersistentDomain(forName: suite) }
        let preferences = Preferences(defaults: defaults)
        preferences.notificationsEnabled = notificationsEnabled
        let view = Form {
            SettingsNotificationsSection(
                preferences: preferences,
                pushRegistrationStatus: .registered,
                notificationPermission: .granted
            )
        }
        .huddleForm()
        try await assertScreenSnapshot(of: view, size: Self.settings, testName: testName, line: line)
    }
}
