import SnapshotTesting
import SwiftUI
import XCTest
@testable import Waddle

#if os(macOS)
import AppKit
#else
import UIKit
#endif

/// The OS release the screen goldens were recorded on. Whole screens draw
/// system chrome (bars, lists, symbols) that changes between releases, so
/// they are only compared on this one; elsewhere the test is skipped.
let screenGoldenOSVersion = 27

/// Asserts a whole screen against its Night and Daylight goldens.
///
/// Unlike `assertHuddleSnapshot`, the view is hosted in a real window, so
/// lists, navigation stacks and toolbars draw as they do in the app.
/// Images are rendered at a fixed 2x, whatever display the test runs on.
@MainActor
func assertScreenSnapshot<V: View>(
    of view: V,
    size: CGSize,
    file: StaticString = #filePath,
    testName: String = #function,
    line: UInt = #line
) async throws {
    try XCTSkipUnless(
        ProcessInfo.processInfo.operatingSystemVersion.majorVersion == screenGoldenOSVersion,
        "Screen goldens are recorded on OS \(screenGoldenOSVersion)"
    )
    for theme in SnapshotTheme.allCases {
        let image = try await renderScreen(view, theme: theme, size: size)
        // A whole screen is mostly ground, so a missing badge is a tiny
        // share of its pixels: demand a near-exact match.
        assertSnapshot(
            of: image,
            as: .image(precision: 0.9995, perceptualPrecision: 0.98),
            named: "\(theme.rawValue)-\(platformName)",
            file: file,
            testName: testName,
            line: line
        )
    }
}

#if os(macOS)
private let platformName = "macOS"
#else
private let platformName = "iOS"
#endif

/// Time for `.task` work, list layout and image decoding to settle.
private let settleTime: Duration = .milliseconds(900)

#if os(macOS)
@MainActor
private func renderScreen<V: View>(_ view: V, theme: SnapshotTheme, size: CGSize) async throws -> NSImage {
    let window = NSWindow(
        contentRect: CGRect(origin: .zero, size: size),
        styleMask: [.titled, .fullSizeContentView],
        backing: .buffered,
        defer: false
    )
    window.isReleasedWhenClosed = false
    window.appearance = NSAppearance(named: theme == .night ? .darkAqua : .aqua)
    window.contentViewController = NSHostingController(rootView: view.frame(width: size.width, height: size.height))
    window.setContentSize(size)
    // Off any screen: the window lays out and draws without being shown.
    window.setFrameOrigin(CGPoint(x: -20_000, y: -20_000))
    window.orderFrontRegardless()
    defer { window.close() }
    try await Task.sleep(for: settleTime)

    // The frame view includes the title bar and toolbar the content draws under.
    let target = try XCTUnwrap(window.contentView?.superview)
    target.layoutSubtreeIfNeeded()
    let bounds = target.bounds
    let rep = try XCTUnwrap(NSBitmapImageRep(
        bitmapDataPlanes: nil,
        pixelsWide: Int(bounds.width * 2),
        pixelsHigh: Int(bounds.height * 2),
        bitsPerSample: 8,
        samplesPerPixel: 4,
        hasAlpha: true,
        isPlanar: false,
        colorSpaceName: .deviceRGB,
        bytesPerRow: 0,
        bitsPerPixel: 0
    ))
    rep.size = bounds.size
    target.cacheDisplay(in: bounds, to: rep)
    let image = NSImage(size: bounds.size)
    image.addRepresentation(rep)
    return image
}
#else
@MainActor
private func renderScreen<V: View>(_ view: V, theme: SnapshotTheme, size: CGSize) async throws -> UIImage {
    let scene = try XCTUnwrap(UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first)
    let window = UIWindow(windowScene: scene)
    window.frame = CGRect(origin: .zero, size: size)
    window.overrideUserInterfaceStyle = theme == .night ? .dark : .light
    window.rootViewController = UIHostingController(rootView: view)
    window.isHidden = false
    defer { window.isHidden = true }
    window.layoutIfNeeded()
    try await Task.sleep(for: settleTime)

    let format = UIGraphicsImageRendererFormat()
    format.scale = 2
    return UIGraphicsImageRenderer(size: size, format: format).image { _ in
        window.drawHierarchy(in: window.bounds, afterScreenUpdates: true)
    }
}
#endif

extension CGSize {
    /// An iPhone in portrait.
    static let phone = CGSize(width: 393, height: 852)
    /// A Mac window, or an iPad in landscape.
    static let desktop = CGSize(width: 1100, height: 720)
}
