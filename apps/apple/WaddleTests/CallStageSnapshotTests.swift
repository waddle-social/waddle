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

    private static func stageControls(
        status: CallSessionStatus, presentation: CallStagePresentation,
        participants: [CallParticipantRow]? = nil, isMicrophoneOn: Bool = true, isCameraOn: Bool = false,
        isHandRaised: Bool? = false, size: CallSurface.Size = .split, isSharingScreen: Bool? = nil,
        isFloating: Bool? = nil, minimises: Bool = false
    ) -> CallControls {
        CallControls(
            devices: .init(
                isMicrophoneOn: isMicrophoneOn, isCameraOn: isCameraOn,
                hasMedia: status.hasMedia, isSharingScreen: isSharingScreen
            ),
            isHandRaised: isHandRaised, viewMode: presentation.viewMode, isSelfViewHidden: presentation.isSelfViewHidden,
            size: size, showsParticipants: participants != nil, canReact: status.hasMedia, isFloating: isFloating,
            minimises: minimises
        )
    }

    private func assertStage(
        title: String = "#design", subtitle: String = "2 others in call", status: CallSessionStatus = .connected,
        tiles: [CallStageTile], presentation: CallStagePresentation = CallStagePresentation(), promotedSpeaker: String? = nil,
        isMicrophoneOn: Bool = true, isCameraOn: Bool = false, isHandRaised: Bool? = false, isFloating: Bool? = nil, minimises: Bool = false,
        issues: [CallMediaIssue] = [],
        checkingIssues: Set<CallMediaIssue.Kind> = [], isChromeHidden: Bool = false,
        stageSize: CallSurface.Size = .split, size: CGSize = panel, testName: String = #function, line: UInt = #line
    ) {
        let controls = Self.stageControls(
            status: status, presentation: presentation, isMicrophoneOn: isMicrophoneOn, isCameraOn: isCameraOn,
            isHandRaised: isHandRaised, size: stageSize, isFloating: isFloating, minimises: minimises
        )
        // SF Symbols in every control: allow for their redraws between OS releases.
        assertHuddleSnapshot(
            precision: 0.97,
            of: CallStageView(
                title: title, subtitle: subtitle, status: status, tiles: tiles, presentation: presentation,
                promotedSpeaker: promotedSpeaker, controls: controls, issues: issues, checkingIssues: checkingIssues,
                isChromeHidden: isChromeHidden, tileSource: Self.cameras, announcesChanges: false,
                perform: { _ in }
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
            isHandRaised: nil, issues: [CallMediaIssue(kind: .camera, reason: .denied)]
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

    /// Full screen: the header and the controls lie over the tiles.
    func testRoomFullScreen() {
        assertStage(
            subtitle: "4 others in call",
            tiles: [Self.you, Self.tile("linus", hasVideo: true), Self.grace, Self.ken, Self.mika],
            stageSize: .immersive, size: CGSize(width: 1000, height: 640)
        )
    }

    /// Full screen with the pointer at rest: the tiles have the window to themselves.
    func testRoomFullScreenAtRest() {
        assertStage(
            subtitle: "4 others in call",
            tiles: [Self.you, Self.tile("linus", hasVideo: true), Self.grace, Self.ken, Self.mika],
            isChromeHidden: true, stageSize: .immersive, size: CGSize(width: 1000, height: 640)
        )
    }

    // MARK: - Over a phone's screen

    private static let phoneRoom = [you, tile("linus", isSpeaking: true, hasVideo: true), tile("grace", isMuted: true), ken]

    /// An iPhone held upright: the stage as it is on a Mac, with one way back to the dock.
    func testRoomOverAPhonesScreen() {
        assertStage(
            subtitle: "3 others in call", tiles: Self.phoneRoom, minimises: true, stageSize: .expanded,
            size: CGSize(width: 393, height: 760)
        )
    }

    /// And on its side: the tiles in one row between the header and the controls.
    func testRoomOverAPhonesScreenOnItsSide() {
        assertStage(
            subtitle: "3 others in call", tiles: Self.phoneRoom, minimises: true, stageSize: .expanded,
            size: CGSize(width: 760, height: 372)
        )
    }

    func testDirectOverAPhonesScreen() {
        assertStage(
            title: "Mika Lindqvist", subtitle: "Video call",
            tiles: [Self.you, Self.tile("Mika Lindqvist", hasVideo: true)], isCameraOn: true, isHandRaised: nil,
            minimises: true, stageSize: .expanded, size: CGSize(width: 393, height: 760)
        )
    }

    /// The picture is in its floating window: the stage says where the call went.
    func testRoomWhileTheCallFloats() {
        assertStage(
            title: "#design", subtitle: "3 others in call", status: .connected,
            tiles: [Self.you, Self.tile("linus", hasVideo: true), Self.grace, Self.ken], isFloating: true
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
        let status: CallSessionStatus = .connected
        let presentation = CallStagePresentation()
        let controls = Self.stageControls(status: status, presentation: presentation, size: .expanded, isSharingScreen: true)
        assertHuddleSnapshot(
            precision: 0.97,
            of: CallStageView(
                title: "#design", subtitle: "2 others in call", status: status,
                tiles: [Self.you, Self.linus, Self.grace], presentation: presentation, controls: controls,
                ownScreen: CallStageTile(
                    identity: Self.you.identity, source: .screen, label: "Your screen", isSelf: true, hasVideo: true
                ),
                tileSource: Self.cameras, announcesChanges: false, perform: { _ in }
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

    // MARK: - Notices about our own devices

    /// Microphone and camera both refused, the camera being checked again; and a share that failed.
    func testRoomWithEveryNotice() {
        assertStage(
            tiles: [Self.you, Self.linus, Self.grace], isMicrophoneOn: false,
            issues: [
                CallMediaIssue(kind: .microphone, reason: .denied), CallMediaIssue(kind: .camera, reason: .denied),
                CallMediaIssue(kind: .screen, reason: .failed),
            ],
            checkingIssues: [.camera], stageSize: .expanded, size: CGSize(width: 1000, height: 760)
        )
    }

    // MARK: - Connection

    private func assertConnection(
        _ quality: CallConnectionQuality, title: String = "#design", size: CGSize = CGSize(width: 760, height: 360),
        testName: String = #function, line: UInt = #line
    ) {
        let status: CallSessionStatus = .connected
        let presentation = CallStagePresentation()
        let controls = Self.stageControls(status: status, presentation: presentation)
        assertHuddleSnapshot(
            precision: 0.97,
            of: CallStageView(
                title: title, subtitle: "2 others in call", status: status,
                tiles: [Self.you, Self.linus, Self.grace], presentation: presentation, controls: controls,
                connection: quality, tileSource: Self.cameras, announcesChanges: false, perform: { _ in }
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
        tiles: [CallStageTile] = roster, coverTiles: Bool, stageSize: CallSurface.Size = .expanded,
        issues: [CallMediaIssue] = [], size: CGSize, testName: String = #function, line: UInt = #line
    ) async throws {
        let status: CallSessionStatus = .connected
        let presentation = CallStagePresentation()
        let participants = Self.rows(of: tiles)
        let controls = Self.stageControls(status: status, presentation: presentation, participants: participants, size: stageSize)
        try await assertScreenSnapshot(
            of: CallStageView(
                title: "#design", subtitle: "3 others in call", status: status, tiles: tiles,
                presentation: presentation, controls: controls, participants: participants, canResetVolumes: true,
                participantsCoverTiles: coverTiles, issues: issues,
                tileSource: Self.cameras, announcesChanges: false, perform: { _ in }
            ),
            size: size, testName: testName, line: line
        )
    }

    func testRoomExpandedWithParticipants() async throws {
        try await assertParticipants(coverTiles: false, size: CGSize(width: 1000, height: 640))
    }

    /// Full screen with the panel open: it holds the header and controls, and stands between them.
    func testRoomFullScreenWithParticipants() async throws {
        try await assertParticipants(coverTiles: false, stageSize: .immersive, size: CGSize(width: 1000, height: 640))
    }

    /// A notice lies under the header in full screen: the panel starts below it, its close button in reach.
    func testRoomFullScreenWithParticipantsUnderANotice() async throws {
        try await assertParticipants(
            coverTiles: false, stageSize: .immersive, issues: [CallMediaIssue(kind: .camera, reason: .denied)],
            size: CGSize(width: 1000, height: 640)
        )
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

    /// Three reactions as they appear, before they rise: each in its own place across the stage.
    func testRoomWithReactionsRising() {
        var ledger = CallReactionLedger()
        let now = Date()
        for emoji in ["👍", "🎉", "❤️"] { ledger.received(emoji, from: nil, at: now) }
        assertHuddleSnapshot(
            precision: 0.97,
            of: CallReactionsField(bursts: ledger.bursts, animates: false)
                .frame(width: Self.panel.width, height: 240)
                .background(Theme.Colors.seaDeep),
            width: Self.panel.width + 2 * Theme.Spacing.l
        )
    }

    // MARK: - Control bar

    private func assertBar(
        isMicrophoneOn: Bool = true, isCameraOn: Bool = false, hasMedia: Bool = true, isHandRaised: Bool? = nil,
        viewMode: CallStagePresentation.ViewMode = .gallery, size: CallSurface.Size = .split,
        isSharingScreen: Bool? = nil, chatUnread: Int? = nil, showsChat: Bool = false, canReact: Bool = false,
        isFloating: Bool? = nil, testName: String = #function, line: UInt = #line
    ) {
        let controls = CallControls(
            devices: .init(
                isMicrophoneOn: isMicrophoneOn, isCameraOn: isCameraOn,
                hasMedia: hasMedia, isSharingScreen: isSharingScreen
            ),
            isHandRaised: isHandRaised, viewMode: viewMode, size: size,
            chatUnread: chatUnread, showsChat: showsChat, canReact: canReact, isFloating: isFloating
        )
        assertHuddleSnapshot(
            precision: 0.97,
            of: CallControlBar(controls: controls, perform: { _ in })
            .padding(Theme.Spacing.l)
            .background(Theme.Colors.seaDeep)
            .environment(\.colorScheme, .dark),
            width: Self.panel.width + 2 * Theme.Spacing.l,
            testName: testName,
            line: line
        )
    }

    /// Someone has a picture: the call can float.
    func testControlBarWithPictureInPicture() {
        assertBar(isHandRaised: false, size: .split, isFloating: false)
    }

    /// A room's call with messages in its chat that nobody here has looked at.
    func testControlBarWithUnreadCallChat() {
        assertBar(isHandRaised: false, size: .expanded, chatUnread: 3)
    }

    /// Everything that keeps its button whatever the width, at the stage's narrowest: a hand, a share in progress, unread chat.
    func testControlBarAtItsFullestInASplitStage() {
        assertBar(isHandRaised: true, size: .split, isSharingScreen: true, chatUnread: 12)
    }

    func testControlBarWithTheCallChatOpen() {
        assertBar(isHandRaised: false, size: .expanded, chatUnread: 0, showsChat: true)
    }

    /// Room for every button: reactions have one of their own.
    func testControlBarWithReactionsInAnExpandedStage() {
        assertBar(isHandRaised: false, size: .expanded, canReact: true)
    }

    /// The 400 pt column of a room's call: reacting goes into More, and sharing and the list of people keep their buttons.
    func testControlBarWithReactionsInASplitRoomStage() {
        assertBar(isHandRaised: false, size: .split, isSharingScreen: false, canReact: true)
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
