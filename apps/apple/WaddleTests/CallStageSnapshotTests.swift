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
        stageSize: CallSurface.Size = .split, size: CGSize = panel, testName: String = #function, line: UInt = #line
    ) {
        // SF Symbols in every control: allow for their redraws between OS releases.
        assertHuddleSnapshot(
            precision: 0.97,
            of: CallStageView(
                title: title, subtitle: subtitle, status: status, tiles: tiles, presentation: presentation,
                promotedSpeaker: promotedSpeaker, size: stageSize, isMicrophoneOn: isMicrophoneOn, isCameraOn: isCameraOn,
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

    /// Expanded over its conversation's pane: room for everyone, and the way to full screen.
    func testRoomExpanded() {
        assertStage(
            subtitle: "4 others in call",
            tiles: [Self.you, Self.tile("linus", hasVideo: true), Self.grace, Self.ken, Self.mika],
            stageSize: .expanded, size: CGSize(width: 1000, height: 640)
        )
    }

    /// Someone shares their screen: it takes the large tile, in gallery view too.
    func testRoomWithASharedScreen() {
        let screen = CallStageTile(
            identity: Self.linus.identity, source: .screen, label: "linus's screen", hasVideo: true,
            screenPublicationID: "TR_screen"
        )
        let tiles = [Self.you, Self.linus, screen, Self.grace]
        var presentation = CallStagePresentation()
        presentation.reconcile(with: tiles)
        assertStage(tiles: tiles, presentation: presentation, stageSize: .expanded, size: CGSize(width: 1000, height: 640))
    }

    /// We share our screen: the strip says so and shows what everyone sees; our screen is not a tile.
    func testRoomWhileWeShareOurScreen() {
        assertHuddleSnapshot(
            precision: 0.97,
            of: CallStageView(
                title: "#design", subtitle: "2 others in call", status: .inCall(since: nil),
                tiles: [Self.you, Self.linus, Self.grace], size: .expanded, isMicrophoneOn: true, isHandRaised: false,
                isSharingScreen: true,
                ownScreen: CallStageTile(
                    identity: Self.you.identity, source: .screen, label: "Your screen", isSelf: true, hasVideo: true
                ),
                tileSource: Self.cameras, announcesChanges: false, actions: Self.actions
            )
            .frame(width: 1000, height: 640),
            width: 1000 + 2 * Theme.Spacing.l
        )
    }

    #if os(macOS)
    func testScreenPicker() async throws {
        let sources = CallScreenSources(
            CallScreenCandidates(
                screens: [
                    .init(number: 1, width: 3024, height: 1964, isMain: true),
                    .init(number: 2, width: 2560, height: 1440, isMain: false),
                ],
                windows: [
                    .init(number: 11, title: "Roadmap — Q1", application: "Keynote", bundleID: "com.apple.iWork.Keynote"),
                    .init(number: 12, title: "waddle — zsh", application: "Terminal", bundleID: "com.apple.Terminal"),
                    .init(number: 13, title: "Pull requests · waddle", application: "Safari", bundleID: "com.apple.Safari"),
                ]
            ),
            ownBundleID: nil
        )
        try await assertScreenSnapshot(
            of: CallScreenPicker(sources: sources, pick: { _ in /* drawn only */ }, cancel: Self.noAction), size: CGSize(width: 420, height: 460)
        )
    }
    #endif

    // MARK: - Connection

    private func assertConnection(
        _ quality: CallConnectionQuality, title: String = "#design", size: CGSize = CGSize(width: 760, height: 360),
        testName: String = #function, line: UInt = #line
    ) {
        assertHuddleSnapshot(
            precision: 0.97,
            of: CallStageView(
                title: title, subtitle: "2 others in call", status: .inCall(since: nil),
                tiles: [Self.you, Self.linus, Self.grace], isMicrophoneOn: true, isHandRaised: false,
                connection: quality, tileSource: Self.cameras, announcesChanges: false, actions: Self.actions
            )
            .frame(width: size.width, height: size.height),
            width: size.width + 2 * Theme.Spacing.l, testName: testName, line: line
        )
    }

    func testAGoodConnectionIsTwoQuietBars() {
        assertConnection(.good)
    }

    func testAPoorConnectionSaysSo() {
        assertConnection(.poor)
    }

    func testALostConnectionSaysSo() {
        assertConnection(.lost)
    }

    /// In the narrow column with a long room name: the name gives way, the warning does not.
    func testALostConnectionInTheNarrowColumn() {
        assertConnection(.lost, title: "#release-train-coordination-and-planning", size: Self.panel)
    }

    /// Before the first measurement: the bars are there, empty, and claim nothing.
    func testAConnectionNotYetMeasured() {
        assertConnection(.unknown)
    }

    // MARK: - Participants

    private static let roster: [CallStageTile] = [
        you, tile("linus", isSpeaking: true, hasVideo: true), tile("grace", isMuted: true, isHandRaised: true), ken,
    ]

    /// Linus turned up, Grace silenced, Ken not yet in the media session.
    private static func rows(of tiles: [CallStageTile]) -> [CallParticipantRow] {
        CallParticipantRows.make(tiles: tiles, audio: audio)
    }

    private static let audio = [
        CallParticipantAudio(identity: you.identity, hasMicrophone: true, volume: 1),
        CallParticipantAudio(identity: roster[1].identity, hasMicrophone: true, volume: 1.4),
        CallParticipantAudio(identity: roster[2].identity, hasMicrophone: true, volume: 0),
    ]

    #if os(macOS)
    /// Drawn in a window: `ImageRenderer` leaves a scroll view and a slider blank.
    private func assertParticipants(
        tiles: [CallStageTile] = roster, coverTiles: Bool, size: CGSize, testName: String = #function, line: UInt = #line
    ) async throws {
        try await assertScreenSnapshot(
            of: CallStageView(
                title: "#design", subtitle: "3 others in call", status: .inCall(since: nil), tiles: tiles,
                size: .expanded, participants: Self.rows(of: tiles), canResetVolumes: true, participantsCoverTiles: coverTiles, isMicrophoneOn: true,
                isHandRaised: false, tileSource: Self.cameras, announcesChanges: false, actions: Self.actions
            ),
            size: size, testName: testName, line: line
        )
    }

    func testRoomExpandedWithParticipants() async throws {
        try await assertParticipants(coverTiles: false, size: CGSize(width: 1000, height: 640))
    }

    /// The "+N more" tile stays a plain button beside the open panel.
    func testRoomFullGalleryBesideParticipants() async throws {
        let crowd = Self.roster + ["mika", "ada", "tim", "joan", "alan", "edsger", "barbara"].map { Self.tile($0) }
        try await assertParticipants(tiles: crowd, coverTiles: false, size: CGSize(width: 1000, height: 640))
    }

    /// Too narrow for both: the panel takes the tiles' place.
    func testRoomParticipantsInANarrowStage() async throws {
        try await assertParticipants(coverTiles: true, size: CGSize(width: 560, height: 640))
    }
    #endif

    // MARK: - Control bar

    private func assertBar(
        isMicrophoneOn: Bool = true, isCameraOn: Bool = false, hasMedia: Bool = true, isHandRaised: Bool? = nil,
        viewMode: CallStagePresentation.ViewMode = .gallery, size: CallSurface.Size = .split,
        isSharingScreen: Bool? = nil, testName: String = #function, line: UInt = #line
    ) {
        assertHuddleSnapshot(
            precision: 0.97,
            of: CallControlBar(
                isMicrophoneOn: isMicrophoneOn, isCameraOn: isCameraOn, hasMedia: hasMedia, isHandRaised: isHandRaised,
                viewMode: viewMode, size: size, isSharingScreen: isSharingScreen, actions: Self.actions
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

    func testControlBarExpanded() {
        assertBar(size: .expanded)
    }

    #if os(macOS)
    // Only a Mac has the button.
    func testControlBarWithScreenShare() {
        assertBar(isHandRaised: false, isSharingScreen: false)
    }

    /// Every button there is, in the narrowest stage: the list of people moves into More, and Stop sharing stays.
    func testControlBarWhileSharingTheScreen() {
        assertBar(isHandRaised: false, size: .expanded, isSharingScreen: true)
    }
    #endif

    func testControlBarFullScreen() {
        assertBar(size: .immersive)
    }
}
