import SwiftUI
import WaddleKit
import XCTest
@testable import Waddle

/// Night and Daylight goldens for a room's call in the dock and for the
/// banner a room shows while its call goes on without us, at iPhone width
/// and, for a call in progress, at its widest.
///
/// To re-record after an intended change, delete the affected files under
/// `__Snapshots__/GroupCallDockSnapshotTests` and run the tests twice.
@MainActor
final class GroupCallDockSnapshotTests: XCTestCase {
    private static let room = BareJID(parsing: "design@muc.waddle.test")!
    private static let audio = CallMedia(audio: true, video: false)
    private static let video = CallMedia(audio: true, video: true)
    /// Buttons in a snapshot are only drawn, never pressed.
    private static let noAction: @MainActor () -> Void = { /* nothing to do */ }
    private static let actions = GroupCallDockActions(
        leave: noAction, toggleMicrophone: noAction, toggleCamera: noAction, toggleHand: noAction
    )
    /// The iPhone 16 width.
    private nonisolated static let phone: CGFloat = 393
    /// The widest card, on the snapshot's padding.
    private static let wide = CallDockView.maxWidth + 2 * Theme.Spacing.l

    private static let me = GroupCallRosterRow(nick: "oyvind", jid: BareJID(parsing: "oyvind@waddle.test"), isSelf: true)
    private static let mika = GroupCallRosterRow(nick: "Mika Lindqvist", jid: BareJID(parsing: "mika@waddle.test"))
    private static let ana = GroupCallRosterRow(nick: "Ana Okafor", jid: BareJID(parsing: "ana@waddle.test"))
    private static let jun = GroupCallRosterRow(nick: "Jun Park", jid: BareJID(parsing: "jun@waddle.test"))

    /// Mika speaking, Ana muted, Jun with a hand up.
    private static var three: [GroupCallRosterRow] {
        var mika = mika, ana = ana, jun = jun
        mika.isSpeaking = true
        ana.isMuted = true
        jun.isHandRaised = true
        return [me, mika, ana, jun]
    }

    private static var crowd: [GroupCallRosterRow] {
        [me, mika, ana, jun] + ["Sami Virtanen", "Lea Novak", "Tomás Ruiz", "Ines Duarte", "Kai Brandt", "Noor Haddad", "Elin Berg"]
            .map { GroupCallRosterRow(nick: $0, jid: nil) }
    }

    private func assertDock(
        _ stage: GroupCallStage, rows: [GroupCallRosterRow], elapsed: Duration? = nil, isMicrophoneOn: Bool = true,
        isCameraOn: Bool = false, isHandRaised: Bool = false, remoteVideo: AnyView? = nil, localVideo: AnyView? = nil,
        width: CGFloat = phone, textSize: DynamicTypeSize = .large, testName: String = #function, line: UInt = #line
    ) {
        // SF Symbols in every control: allow for their redraws between OS releases.
        assertHuddleSnapshot(
            precision: 0.97,
            of: GroupCallDockView(
                stage: stage, roomName: "design", rows: rows, elapsed: elapsed, isMicrophoneOn: isMicrophoneOn,
                isCameraOn: isCameraOn, isHandRaised: isHandRaised, remoteVideo: remoteVideo, localVideo: localVideo,
                announcesChanges: false, actions: Self.actions
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

    /// Only Leave works until the mixer has let us in.
    func testJoining() {
        assertDock(.joining(room: Self.room, media: Self.audio), rows: [Self.mika, Self.ana], isMicrophoneOn: false)
    }

    func testInCallWithThreeOthers() {
        assertDock(.inCall(room: Self.room, media: Self.audio), rows: Self.three, elapsed: .seconds(4 * 60 + 12))
    }

    func testInCallWithThreeOthersWide() {
        assertDock(
            .inCall(room: Self.room, media: Self.audio), rows: Self.three, elapsed: .seconds(4 * 60 + 12), width: Self.wide
        )
    }

    func testInCallAlone() {
        assertDock(.inCall(room: Self.room, media: Self.audio), rows: [Self.me], elapsed: .seconds(9))
    }

    func testInCallMutedWithOurHandRaised() {
        var me = Self.me
        me.isMuted = true
        me.isHandRaised = true
        assertDock(
            .inCall(room: Self.room, media: Self.audio), rows: [me, Self.mika, Self.ana], elapsed: .seconds(75),
            isMicrophoneOn: false, isHandRaised: true
        )
    }

    /// More faces than fit end with a count of the rest.
    func testInCallWithMorePeopleThanFit() {
        assertDock(.inCall(room: Self.room, media: Self.audio), rows: Self.crowd, elapsed: .seconds(30 * 60))
    }

    func testInCallWithVideo() {
        assertDock(
            .inCall(room: Self.room, media: Self.video), rows: Self.three, elapsed: .seconds(3725), isCameraOn: true,
            remoteVideo: Self.camera(Theme.Colors.seaShallow), localVideo: Self.camera(Theme.Colors.halo)
        )
    }

    /// The SFU's roster and voices are gone until it is back; presence remains.
    func testReconnecting() {
        assertDock(.reconnecting(room: Self.room, media: Self.audio), rows: Self.three.map { row in
            var row = row
            row.isSpeaking = false
            return row
        })
    }

    /// Large text stacks and wraps; nothing is cut off.
    func testInCallAtAccessibilityTextSize() {
        assertDock(
            .inCall(room: Self.room, media: Self.audio), rows: Self.three, elapsed: .seconds(4 * 60 + 12),
            textSize: .accessibility3
        )
    }

    // MARK: - Room banner

    private func assertBanner(_ rows: [GroupCallRosterRow], testName: String = #function, line: UInt = #line) {
        assertHuddleSnapshot(
            precision: 0.97,
            of: RoomCallBannerView(rows: rows, onJoin: Self.noAction),
            width: Self.phone,
            testName: testName,
            line: line
        )
    }

    func testRoomBannerWithTwoPeople() {
        assertBanner([Self.mika, Self.ana])
    }

    func testRoomBannerWithManyPeople() {
        assertBanner(Array(Self.crowd.dropFirst()))
    }
}
