import SwiftUI
import WaddleKit
import XCTest

@testable import Waddle

/// The sidebar's current-call card, on the rail it sits in.
@MainActor
final class CurrentCallCardSnapshotTests: XCTestCase {
    private static let noAction: @MainActor () -> Void = { /* nothing to do */ }
    private static let actions = CurrentCallCard.Actions(
        returnToCall: noAction, expand: noAction, toggleMicrophone: noAction, toggleCamera: noAction, hangUp: noAction
    )
    /// The sidebar at its ideal width.
    private static let width: CGFloat = 280

    private func assertCard(_ card: CurrentCallCard, testName: String = #function, line: UInt = #line) {
        // SF Symbols in every control: allow for their redraws between OS releases.
        assertHuddleSnapshot(
            precision: 0.97,
            of: card
                .padding(Theme.Spacing.m)
                .background(Theme.Colors.bgRail)
                .environment(\.huddleGround, Theme.Colors.bgRail)
                .environment(\.colorScheme, .dark),
            width: Self.width,
            testName: testName,
            line: line
        )
    }

    func testDirectVideoCallConnected() {
        assertCard(CurrentCallCard(
            title: "priya",
            summary: CallCardSummary.line(status: .connected, kind: .direct(CallMedia(audio: true, video: true))),
            isMicrophoneOn: true, isCameraOn: true, actions: Self.actions
        ))
    }

    func testRoomCallMuted() {
        assertCard(CurrentCallCard(
            title: "#design", summary: CallCardSummary.line(status: .connected, kind: .room(count: 4)),
            isMicrophoneOn: false, isCameraOn: false, actions: Self.actions
        ))
    }

    func testConnecting() {
        assertCard(CurrentCallCard(
            title: "release-train with a long name",
            summary: CallCardSummary.line(status: .connecting, kind: .room(count: 1)),
            isConnected: false, actions: Self.actions
        ))
    }
}
