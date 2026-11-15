import CoreText
import SwiftUI
import XCTest
@testable import Waddle

@MainActor
final class DesignFoundationTests: XCTestCase {
    /// A missing or misnamed font file falls back to the system font
    /// without an error, so check each role resolves to its own family.
    func testEveryFontRoleResolvesToItsBundledFamily() {
        for role in HuddleFont.allCases {
            let font = HuddleFonts.font(role, size: 16, weight: 400)
            XCTAssertEqual(CTFontCopyFamilyName(font) as String, role.familyName)
        }
    }

    func testWeightIsAppliedOnTheVariationAxis() throws {
        let weightAxis = NSNumber(value: 0x7767_6874)
        for role in HuddleFont.allCases {
            let font = HuddleFonts.font(role, size: 16, weight: 700)
            let variation = try XCTUnwrap(CTFontCopyVariation(font) as? [NSNumber: NSNumber])
            XCTAssertEqual(variation[weightAxis]?.doubleValue, 700, "\(role)")
        }
    }

    /// Reference indices from the web client's hash (`AppAvatar.vue`):
    /// djb2 over code points, unsigned 32-bit, modulo six tints.
    func testAvatarTintMatchesTheWebHash() {
        let expected = ["Mika": 3, "Jo Hartmann": 3, "Priya": 0, "🐧 Pingu": 1, "an": 2, "Åse Ørn": 4]
        for (name, index) in expected {
            XCTAssertEqual(AvatarView.tintIndex(for: name, count: 6), index, name)
        }
        XCTAssertEqual(Theme.Colors.avatarTints.count, 6)
    }

    /// Token colours must follow the theme, or Night and Daylight would
    /// render alike.
    func testTokenColoursDifferBetweenNightAndDaylight() throws {
        let swatch = Rectangle().fill(Theme.Colors.surface).frame(width: 8, height: 8)
        let night = try XCTUnwrap(renderHuddle(swatch, theme: .night, width: 40))
        let daylight = try XCTUnwrap(renderHuddle(swatch, theme: .daylight, width: 40))
        XCTAssertNotEqual(night.dataProvider?.data as Data?, daylight.dataProvider?.data as Data?)
    }

    func testLineSpacingNeverGoesNegative() {
        for style in [Theme.Typography.hero, Theme.Typography.badge, Theme.Typography.message] {
            XCTAssertGreaterThanOrEqual(style.resolved().lineSpacing, 0)
        }
    }
}
