#if os(macOS)
import SwiftUI
import XCTest
@testable import Waddle
import WaddleKit

/// The open account menu in Night and Daylight, including each theme icon.
@MainActor
final class SidebarAccountMenuSnapshotTests: XCTestCase {
    func testSystemTheme() throws {
        try assertMenu(appearance: .system)
    }

    func testNightTheme() throws {
        try assertMenu(appearance: .dark)
    }

    func testDaylightTheme() throws {
        try assertMenu(appearance: .light)
    }

    private func assertMenu(appearance: AppearancePreference, testName: String = #function, line: UInt = #line) throws {
        let suite = "account-menu-snapshots.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let preferences = Preferences(defaults: defaults)
        preferences.appearance = appearance
        let view = SampleSession().host(SidebarAccountMenu(preferences: preferences))
        assertHuddleSnapshot(of: view, width: 332, testName: testName, line: line)
    }
}
#endif
