import SwiftUI
import WaddleKit
import XCTest
@testable import Waddle

/// Night and Daylight goldens for every stage of the call dock, at iPhone
/// width and, for a call in progress, at its widest.
///
/// To re-record after an intended change, delete the affected files under
/// `__Snapshots__/CallDockSnapshotTests` and run the tests twice.
@MainActor
final class CallDockSnapshotTests: XCTestCase {
    private static let peer = JID(parsing: "mika@waddle.test/phone")!
    private static let audio = CallMedia(audio: true, video: false)
    private static let video = CallMedia(audio: true, video: true)
    /// Buttons in a snapshot are only drawn, never pressed.
    private static let noAction: @MainActor () -> Void = { /* nothing to do */ }
    private static let actions = CallDockActions(
        accept: noAction, decline: noAction, hangUp: noAction,
        toggleMicrophone: noAction, toggleCamera: noAction, dismiss: noAction
    )
    /// The iPhone 16 width.
    private nonisolated static let phone: CGFloat = 393
    /// The widest card, on the snapshot's padding.
    private static let wide = CallDockView.maxWidth + 2 * Theme.Spacing.l

    private func assertDock(
        _ stage: CallStage, elapsed: Duration? = nil, isMicrophoneOn: Bool = true, isCameraOn: Bool = false,
        remoteVideo: AnyView? = nil, localVideo: AnyView? = nil, width: CGFloat = phone,
        textSize: DynamicTypeSize = .large, testName: String = #function, line: UInt = #line
    ) {
        // SF Symbols in every control: allow for their redraws between OS releases.
        assertHuddleSnapshot(
            precision: 0.97,
            of: CallDockView(
                stage: stage, peerName: "Mika Lindqvist", elapsed: elapsed, isMicrophoneOn: isMicrophoneOn,
                isCameraOn: isCameraOn, remoteVideo: remoteVideo, localVideo: localVideo, actions: Self.actions
            )
            .dynamicTypeSize(textSize),
            width: width,
            testName: testName,
            line: line
        )
    }

    /// Stands in for a camera: a flat colour where the video would be.
    private static func camera(_ color: Color) -> AnyView {
        AnyView(color)
    }

    func testIncomingAudio() {
        assertDock(.incoming(from: Self.peer, media: Self.audio, isAccepting: false))
    }

    func testIncomingVideoWhileAccepting() {
        assertDock(.incoming(from: Self.peer, media: Self.video, isAccepting: true))
    }

    func testCalling() {
        assertDock(.calling(to: Self.peer.bare, media: Self.audio))
    }

    func testRinging() {
        assertDock(.ringing(to: Self.peer.bare, media: Self.video))
    }

    func testInCallMuted() {
        assertDock(.inCall(peer: Self.peer.bare, media: Self.audio), elapsed: .seconds(4 * 60 + 12), isMicrophoneOn: false)
    }

    func testInCallMutedWide() {
        assertDock(
            .inCall(peer: Self.peer.bare, media: Self.audio), elapsed: .seconds(4 * 60 + 12), isMicrophoneOn: false,
            width: Self.wide
        )
    }

    /// Large text stacks and wraps; nothing is cut off.
    func testIncomingAtAccessibilityTextSize() {
        assertDock(
            .incoming(from: Self.peer, media: Self.video, isAccepting: false), textSize: .accessibility3
        )
    }

    func testInCallWithVideo() {
        assertDock(
            .inCall(peer: Self.peer.bare, media: Self.video), elapsed: .seconds(3725), isCameraOn: true,
            remoteVideo: Self.camera(Theme.Colors.seaShallow), localVideo: Self.camera(Theme.Colors.halo)
        )
    }

    func testInCallWithVideoWide() {
        assertDock(
            .inCall(peer: Self.peer.bare, media: Self.video), elapsed: .seconds(3725), isCameraOn: true,
            remoteVideo: Self.camera(Theme.Colors.seaShallow), localVideo: Self.camera(Theme.Colors.halo), width: Self.wide
        )
    }

    func testInCallWithOnlyOurCamera() {
        assertDock(
            .inCall(peer: Self.peer.bare, media: Self.video), elapsed: .seconds(9), isCameraOn: true,
            localVideo: Self.camera(Theme.Colors.halo)
        )
    }

    func testReconnecting() {
        assertDock(.reconnecting(peer: Self.peer.bare, media: Self.audio))
    }

    func testEnded() {
        assertDock(.ended(.finished(.connectivityError)))
    }
}
