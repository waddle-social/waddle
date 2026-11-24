import SwiftUI
import WaddleKit
import XCTest
@testable import Waddle

/// Night and Daylight goldens for a call's stage and its control bar, at
/// the size of the web huddle column. The stage keeps the Night ground in
/// both, so the two differ only in the page around it.
///
/// To re-record after an intended change, delete the affected files under
/// `__Snapshots__/CallStageSnapshotTests` and run the tests twice.
@MainActor
final class CallStageSnapshotTests: XCTestCase {
    /// Buttons in a snapshot are only drawn, never pressed.
    private static let noAction: @MainActor () -> Void = { /* nothing to do */ }
    private static let actions = CallStageActions(
        toggleMicrophone: noAction, toggleCamera: noAction, toggleHand: noAction, setViewMode: { _ in /* drawn only */ },
        toggleSelfView: noAction, hangUp: noAction
    )
    /// Stands in for a camera: a flat colour where the video would be.
    private static let cameras = CallTileSource(
        video: { tile in AnyView(tile.isSelf ? Theme.Colors.halo : Theme.Colors.seaShallow) },
        togglePin: { _ in /* drawn only */ }
    )
    /// The huddle column of the Penpot frames: 400 wide in a 900 tall window.
    private nonisolated static let panel = CGSize(width: CallSurface.stageWidth, height: 900)

    private static let you = CallStageTile(identity: "ada@waddle.test/mac", source: .camera, label: "You", isSelf: true)
    private static let linus = tile("linus")
    private static let grace = tile("grace")
    private static let ken = tile("ken")
    private static let mika = tile("mika")

    private static func tile(
        _ name: String, isMuted: Bool = false, isHandRaised: Bool = false, isSpeaking: Bool = false, hasVideo: Bool = false
    ) -> CallStageTile {
        CallStageTile(
            identity: "\(name)@waddle.test/web", source: .camera, label: name, isMuted: isMuted,
            isHandRaised: isHandRaised, isSpeaking: isSpeaking, hasVideo: hasVideo
        )
    }

    private func assertStage(
        title: String = "#design", subtitle: String = "2 others in call", status: CallStageStatus = .inCall(since: nil),
        tiles: [CallStageTile], presentation: CallStagePresentation = CallStagePresentation(), promotedSpeaker: String? = nil,
        isMicrophoneOn: Bool = true, isCameraOn: Bool = false, isHandRaised: Bool? = false, notice: String? = nil,
        size: CGSize = panel, testName: String = #function, line: UInt = #line
    ) {
        // SF Symbols in every control: allow for their redraws between OS releases.
        assertHuddleSnapshot(
            precision: 0.97,
            of: CallStageView(
                title: title, subtitle: subtitle, status: status, tiles: tiles, presentation: presentation,
                promotedSpeaker: promotedSpeaker, isMicrophoneOn: isMicrophoneOn, isCameraOn: isCameraOn,
                isHandRaised: isHandRaised, notice: notice, tileSource: Self.cameras, announcesChanges: false,
                actions: Self.actions
            )
            .frame(width: size.width, height: size.height),
            width: size.width + 2 * Theme.Spacing.l,
            testName: testName,
            line: line
        )
    }

    // MARK: - 1:1

    /// The Penpot `call-active` frame: both cameras off while media connects.
    func testDirectConnecting() {
        assertStage(
            title: "linus", subtitle: "Video call", status: .connecting, tiles: [Self.you, Self.linus],
            isMicrophoneOn: false, isHandRaised: nil
        )
    }

    func testDirectInCallWithCamerasOff() {
        assertStage(title: "linus", subtitle: "Video call", tiles: [Self.you, Self.linus], isHandRaised: nil)
    }

    func testDirectWithCamerasOnAndOurMicrophoneOff() {
        var you = Self.you
        you = CallStageTile(identity: you.identity, source: .camera, label: "You", isSelf: true, isMuted: true, hasVideo: true)
        assertStage(
            title: "linus", subtitle: "Video call", tiles: [you, Self.tile("linus", hasVideo: true)],
            isMicrophoneOn: false, isCameraOn: true, isHandRaised: nil
        )
    }

    func testDirectReconnectingWithTheCameraRefused() {
        assertStage(
            title: "linus", subtitle: "Voice call", status: .reconnecting, tiles: [Self.you, Self.linus],
            isHandRaised: nil, notice: "Waddle is not allowed to use the camera. Change that in Settings."
        )
    }

    // MARK: - Rooms

    /// The Penpot `huddle-room` frame: the column has room for two cells, so
    /// the second counts everyone else.
    func testRoomWithFourMore() {
        assertStage(
            subtitle: "4 others in call", status: .connecting,
            tiles: [Self.you, Self.linus, Self.grace, Self.ken, Self.mika], isMicrophoneOn: false
        )
    }

    func testRoomTileSpeakingWithHandRaisedAndMuted() {
        assertStage(
            subtitle: "1 other in call",
            tiles: [Self.you, Self.tile("linus", isMuted: true, isHandRaised: true, isSpeaking: true)],
            isHandRaised: true
        )
    }

    func testRoomSpeakerView() {
        assertStage(
            tiles: [Self.you, Self.linus, Self.tile("grace", isSpeaking: true)],
            presentation: CallStagePresentation(viewMode: .speaker), promotedSpeaker: Self.grace.identity
        )
    }

    func testRoomPinnedTile() {
        assertStage(
            tiles: [Self.you, Self.tile("linus", isMuted: true), Self.grace],
            presentation: CallStagePresentation(pinnedTileID: Self.linus.id)
        )
    }

    func testRoomWithOurOwnViewHidden() {
        assertStage(
            tiles: [Self.you, Self.linus, Self.grace], presentation: CallStagePresentation(isSelfViewHidden: true)
        )
    }

    /// Above a conversation too narrow to stand beside: wide and short, so the tiles sit side by side.
    func testRoomAboveItsConversation() {
        assertStage(
            subtitle: "3 others in call", tiles: [Self.you, Self.linus, Self.grace, Self.ken],
            size: CGSize(width: 760, height: 360)
        )
    }

    // MARK: - Control bar

    private func assertBar(
        isMicrophoneOn: Bool = true, isCameraOn: Bool = false, hasMedia: Bool = true, isHandRaised: Bool? = nil,
        viewMode: CallStagePresentation.ViewMode = .gallery, testName: String = #function, line: UInt = #line
    ) {
        assertHuddleSnapshot(
            precision: 0.97,
            of: CallControlBar(
                isMicrophoneOn: isMicrophoneOn, isCameraOn: isCameraOn, hasMedia: hasMedia, isHandRaised: isHandRaised,
                viewMode: viewMode, actions: Self.actions
            )
            .padding(Theme.Spacing.l)
            .background(Theme.Colors.seaDeep)
            .environment(\.colorScheme, .dark),
            width: Self.panel.width + 2 * Theme.Spacing.l,
            testName: testName,
            line: line
        )
    }

    func testControlBarDirect() {
        assertBar()
    }

    func testControlBarDirectConnecting() {
        assertBar(isMicrophoneOn: false, hasMedia: false)
    }

    func testControlBarRoom() {
        assertBar(isMicrophoneOn: false, isCameraOn: true, isHandRaised: true, viewMode: .speaker)
    }
}
