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
/// lists, forms, text fields, navigation stacks and toolbars draw as they
/// do in the app. Images are rendered at a fixed 2x, whatever display the
/// test runs on. With `asSheet`, the view is presented as a sheet over an
/// empty window, the way a sheet's toolbar and size are decided: on a Mac
/// only the sheet is captured, on iOS the whole window under it. A sheet
/// is captured without keyboard focus, so no caret blinks and a Mac draws
/// it as an inactive window whatever else the test run has open. The view
/// gets the app's tint, as both shells give it, so system controls draw in
/// the action colour rather than system blue.
///
/// `precision` is the share of pixels that must match; lower it only for
/// what moves on its own, such as a spinner.
@MainActor
func assertScreenSnapshot<V: View>(
    of view: V,
    size: CGSize,
    asSheet: Bool = false,
    precision: Float = 0.9995,
    file: StaticString = #filePath,
    testName: String = #function,
    line: UInt = #line
) async throws {
    try XCTSkipUnless(
        ProcessInfo.processInfo.operatingSystemVersion.majorVersion == screenGoldenOSVersion,
        "Screen goldens are recorded on OS \(screenGoldenOSVersion)"
    )
    let view = view.tint(Theme.Colors.action)
    for theme in SnapshotTheme.allCases {
        let image = asSheet
            ? try await renderScreen(SheetPresenter(sheet: view), theme: theme, size: size, capturesSheet: true)
            : try await renderScreen(view, theme: theme, size: size)
        // A whole screen is mostly ground, so a missing badge is a tiny
        // share of its pixels: demand a near-exact match.
        assertSnapshot(
            of: image,
            as: .image(precision: precision, perceptualPrecision: 0.98),
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

/// An empty window ground with `sheet` presented over it.
private struct SheetPresenter<Sheet: View>: View {
    let sheet: Sheet

    var body: some View {
        Theme.Colors.bg
            .sheet(isPresented: .constant(true)) { sheet }
    }
}

#if os(macOS)
@MainActor
private func renderScreen<V: View>(
    _ view: V, theme: SnapshotTheme, size: CGSize, capturesSheet: Bool = false
) async throws -> NSImage {
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
    placeOffScreen(window)
    defer { window.close() }
    try await Task.sleep(for: settleTime)
    if capturesSheet {
        // Another window takes key, so the sheet's controls, selection and
        // caret draw the same whether or not the test app is active.
        let keyHolder = NSWindow(
            contentRect: CGRect(x: -20_000, y: -20_000, width: 10, height: 10),
            styleMask: [.titled],
            backing: .buffered,
            defer: false
        )
        keyHolder.isReleasedWhenClosed = false
        keyHolder.makeKeyAndOrderFront(nil)
        defer { keyHolder.close() }
        try await Task.sleep(for: .milliseconds(200))
        return try capture(window.attachedSheet)
    }
    return try capture(window)
}

/// Puts the window off any screen, so it lays out and draws without being
/// shown, and beside the sharpest display. A window off screen is drawn at
/// the scale of the display nearest to it; beside a 1x monitor its text
/// would be drawn at 1x and scaled up, which no 2x golden matches.
@MainActor
private func placeOffScreen(_ window: NSWindow) {
    let far: CGFloat = 20_000
    let fallback = CGPoint(x: -far, y: -far)
    window.setFrameOrigin(fallback)
    window.orderFrontRegardless()
    guard let sharpest = NSScreen.screens.max(by: { $0.backingScaleFactor < $1.backingScaleFactor }) else { return }
    let frame = sharpest.frame
    let besideSharpest = [
        CGPoint(x: frame.midX, y: frame.minY - far),
        CGPoint(x: frame.maxX + far, y: frame.midY),
        CGPoint(x: frame.minX - far, y: frame.midY),
        CGPoint(x: frame.midX, y: frame.maxY + far),
    ]
    for origin in besideSharpest where window.backingScaleFactor != sharpest.backingScaleFactor {
        window.setFrameOrigin(origin)
    }
    if window.backingScaleFactor != sharpest.backingScaleFactor {
        window.setFrameOrigin(fallback)
    }
}

/// The window's frame view at 2x.
@MainActor
private func capture(_ captured: NSWindow?) throws -> NSImage {
    let captured = try XCTUnwrap(captured, "No sheet was presented")
    // The frame view includes the title bar and toolbar the content draws under.
    let target = try XCTUnwrap(captured.contentView?.superview)
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
private func renderScreen<V: View>(
    _ view: V, theme: SnapshotTheme, size: CGSize, capturesSheet: Bool = false
) async throws -> UIImage {
    let scene = try XCTUnwrap(UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first)
    let window = UIWindow(windowScene: scene)
    window.frame = CGRect(origin: .zero, size: size)
    window.overrideUserInterfaceStyle = theme == .night ? .dark : .light
    window.rootViewController = UIHostingController(rootView: view)
    window.isHidden = false
    defer { window.isHidden = true }
    window.layoutIfNeeded()
    try await Task.sleep(for: settleTime)
    if capturesSheet {
        // Without focus no caret blinks and no keyboard moves the sheet.
        window.endEditing(true)
        try await Task.sleep(for: settleTime)
    }

    let format = UIGraphicsImageRendererFormat()
    format.scale = 2
    // 8-bit sRGB, as the goldens are stored. The automatic range can
    // render glass in extended colour, which then never matches the PNG
    // read back from disk.
    format.preferredRange = .standard
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
    /// A wide Mac window, where the conversation pane is wider than any
    /// sensible line length.
    static let wideDesktop = CGSize(width: 1680, height: 720)
}
