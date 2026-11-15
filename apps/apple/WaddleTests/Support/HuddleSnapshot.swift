import SnapshotTesting
import SwiftUI
import XCTest
@testable import Waddle

/// The two Polar Huddle themes a snapshot is taken in.
enum SnapshotTheme: String, CaseIterable {
    case night
    case daylight

    var colorScheme: ColorScheme {
        self == .night ? .dark : .light
    }
}

/// Renders `view` on the page ground in `theme` at a fixed 2x scale, so
/// the image does not depend on the simulator or display it runs on.
@MainActor
func renderHuddle<V: View>(_ view: V, theme: SnapshotTheme, width: CGFloat) -> CGImage? {
    let content = view
        .padding(Theme.Spacing.l)
        .frame(width: width, alignment: .leading)
        .background(Theme.Colors.bg)
        .environment(\.colorScheme, theme.colorScheme)
    let renderer = ImageRenderer(content: content)
    renderer.scale = 2
    return renderer.cgImage
}

/// Asserts `view` against its Night and Daylight goldens. Goldens are per
/// platform: text rasterises differently on iOS and macOS.
@MainActor
func assertHuddleSnapshot<V: View>(
    of view: V,
    width: CGFloat = 390,
    file: StaticString = #filePath,
    testName: String = #function,
    line: UInt = #line
) {
    for theme in SnapshotTheme.allCases {
        guard let image = renderHuddle(view, theme: theme, width: width) else {
            XCTFail("Could not render the \(theme.rawValue) snapshot", file: file, line: line)
            continue
        }
        #if os(macOS)
        assertSnapshot(
            of: NSImage(cgImage: image, size: NSSize(width: image.width / 2, height: image.height / 2)),
            as: .image(precision: 0.99, perceptualPrecision: 0.98),
            named: "\(theme.rawValue)-macOS",
            file: file,
            testName: testName,
            line: line
        )
        #else
        assertSnapshot(
            of: UIImage(cgImage: image, scale: 2, orientation: .up),
            as: .image(precision: 0.99, perceptualPrecision: 0.98),
            named: "\(theme.rawValue)-iOS",
            file: file,
            testName: testName,
            line: line
        )
        #endif
    }
}
