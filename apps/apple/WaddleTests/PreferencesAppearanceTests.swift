import XCTest
@testable import Waddle

/// Settings lists the appearances in the web's order; a new one must not
/// drop out of it.
final class PreferencesAppearanceTests: XCTestCase {
    func testSettingsListsEveryAppearanceOnce() {
        let order = Preferences.Appearance.settingsOrder
        XCTAssertEqual(order.count, Preferences.Appearance.allCases.count)
        XCTAssertEqual(Set(order), Set(Preferences.Appearance.allCases))
        XCTAssertEqual(order.map(\.title), ["System", "Night", "Daylight"])
    }
}
