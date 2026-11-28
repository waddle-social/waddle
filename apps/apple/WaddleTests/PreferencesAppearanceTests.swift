import XCTest
@testable import Waddle
import WaddleKit

final class PreferencesAppearanceTests: XCTestCase {
    func testSettingsListsAppearancesInWebOrder() {
        XCTAssertEqual(AppearancePreference.allCases.map(\.title), ["System", "Night", "Daylight"])
    }

    @MainActor
    func testCyclingUsesTheExistingSavedAppearance() throws {
        let suite = "appearance-tests.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let key = "waddle.apple.appearance"
        defaults.set("dark", forKey: key)

        let preferences = Preferences(defaults: defaults)
        XCTAssertEqual(preferences.appearance, .dark)
        preferences.appearance = preferences.appearance.next

        XCTAssertEqual(defaults.string(forKey: key), "light")
        XCTAssertEqual(Preferences(defaults: defaults).appearance, .light)
    }
}
