import SwiftUI
import WaddleKit
import XCTest
@testable import Waddle

/// Night and Daylight goldens for a call in the window it floats in, at
/// the size the web asks for. It keeps the Night ground in both.
@MainActor
final class CallFloatingSnapshotTests: XCTestCase {
    /// Stands in for a camera: a flat colour where the video would be.
    private static let cameras = CallTileSource(
        video: { _ in AnyView(Theme.Colors.seaShallow) }, togglePin: { _ in /* drawn only */ }
    )

    private func assertFloating(
        tile: CallStageTile?, devices: CallControls.Devices, testName: String = #function, line: UInt = #line
    ) {
        assertHuddleSnapshot(
            precision: 0.97,
            of: CallFloatingView(tile: tile, devices: devices, tileSource: Self.cameras, perform: { _ in })
                .frame(width: 360, height: 240),
            // The helper pads what it draws.
            width: 360 + 2 * Theme.Spacing.l, testName: testName, line: line
        )
    }

    func testSomeoneElsesCamera() {
        assertFloating(
            tile: CallStageTile(identity: "linus@waddle.test/web", source: .camera, label: "linus", isSpeaking: true, hasVideo: true),
            devices: .init(isMicrophoneOn: true, isCameraOn: true)
        )
    }

    /// The picture that floated has gone, and nobody else has one.
    func testNobodyWithAPictureAndOursOff() {
        assertFloating(tile: nil, devices: .init(isMicrophoneOn: false, isCameraOn: false))
    }
}
