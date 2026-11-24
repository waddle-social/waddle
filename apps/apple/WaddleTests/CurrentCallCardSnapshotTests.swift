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
    /// The sidebar's widths (`SplitShell`): the one it opens at and the narrowest it can be dragged to.
    private static let idealSidebar: CGFloat = 280
    private static let narrowestSidebar: CGFloat = 240

    /// The card as `CurrentCallCardHost` places it in a sidebar `sidebar` wide.
    private func assertCard(
        _ card: CurrentCallCard, sidebar: CGFloat = idealSidebar, testName: String = #function, line: UInt = #line
    ) {
        // SF Symbols in every control: allow for their redraws between OS releases.
        assertHuddleSnapshot(
            precision: 0.97,
            of: card
                .padding(.horizontal, Theme.Spacing.m)
                .padding(.vertical, Theme.Spacing.s)
                .frame(width: sidebar)
                .background(Theme.Colors.bgRail)
                .environment(\.huddleGround, Theme.Colors.bgRail)
                .environment(\.colorScheme, .dark),
            width: sidebar + 2 * Theme.Spacing.l,
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
            isConnected: false, hasMedia: false, actions: Self.actions
        ))
    }

    /// Everything stays inside the narrowest sidebar.
    func testInTheNarrowestSidebar() {
        assertCard(
            CurrentCallCard(
                title: "#design", summary: CallCardSummary.line(status: .connected, kind: .room(count: 4)),
                isMicrophoneOn: true, actions: Self.actions
            ),
            sidebar: Self.narrowestSidebar
        )
    }

    func testCameraRefused() {
        assertCard(CurrentCallCard(
            title: "priya",
            summary: CallCardSummary.line(status: .connected, kind: .direct(CallMedia(audio: true, video: true))),
            isMicrophoneOn: true,
            notice: "Waddle is not allowed to use the camera. Change that in Settings.", actions: Self.actions
        ))
    }

    func testWhileSharingTheScreen() {
        assertCard(CurrentCallCard(
            title: "#design", summary: CallCardSummary.line(status: .connected, kind: .room(count: 4)),
            isMicrophoneOn: true, isSharingScreen: true, actions: Self.actions
        ))
    }
}
