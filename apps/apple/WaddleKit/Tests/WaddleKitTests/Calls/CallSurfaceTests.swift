import Foundation
import Testing

@testable import WaddleKit

@Suite("Call surface") struct CallSurfaceTests {
    private static let peer = JID(parsing: "bob@waddle.test/web")!
    private static let media = CallMedia(audio: true, video: false)
    private static let dockStages: [CallStage] = [
        .incoming(from: peer, media: media, isAccepting: false),
        .calling(to: peer.bare, media: media),
        .ringing(to: peer.bare, media: media),
        .ended(.timeout),
    ]
    private static let sessionStages: [CallStage] = [
        .connecting(peer: peer.bare, media: media),
        .inCall(peer: peer.bare, media: media),
        .reconnecting(peer: peer.bare, media: media),
    ]
    private static let room = BareJID(parsing: "lounge@rooms.waddle.test")!
    private static let dockRoomStages: [GroupCallStage] = [
        .joining(room: room, media: media),
    ]
    private static let sessionRoomStages: [GroupCallStage] = [
        .connecting(room: room, media: media),
        .inCall(room: room, media: media),
        .reconnecting(room: room, media: media),
    ]

    @Test(arguments: [CallSurface.Size.split, .expanded, .immersive])
    func phoneCallsAlwaysUseTheDock(size: CallSurface.Size) {
        let sessions = (Self.dockStages + Self.sessionStages).map(\.isInSession)
            + (Self.dockRoomStages + Self.sessionRoomStages).map(\.isInSession)
        for isInSession in sessions {
            for isOpen in [true, false] {
                #expect(CallSurface.placement(
                    isInSession: isInSession, conversationIsOpen: isOpen, platform: .phone, size: size
                ) == .dock)
            }
        }
    }

    @Test(arguments: [CallSurface.Size.split, .expanded, .immersive])
    func onlySessionsCanLeaveTheDockOnRegularWidth(size: CallSurface.Size) {
        let sessions = Self.dockStages.map(\.isInSession) + Self.dockRoomStages.map(\.isInSession)
        for isInSession in sessions {
            for isOpen in [true, false] {
                #expect(CallSurface.placement(
                    isInSession: isInSession, conversationIsOpen: isOpen, platform: .regularWidth, size: size
                ) == .dock)
            }
        }
    }

    @Test(arguments: [
        (CallSurface.Size.split, CallSurface.split),
        (.expanded, .expanded),
        (.immersive, .fullscreen),
    ])
    func regularWidthSessionsFollowTheConversation(size: CallSurface.Size, surface: CallSurface) {
        let sessions = Self.sessionStages.map(\.isInSession) + Self.sessionRoomStages.map(\.isInSession)
        for isInSession in sessions {
            #expect(CallSurface.placement(
                isInSession: isInSession, conversationIsOpen: true, platform: .regularWidth, size: size
            ) == surface)
            #expect(CallSurface.placement(
                isInSession: isInSession, conversationIsOpen: false, platform: .regularWidth, size: size
            ) == .sidebarCard)
        }
    }

    @Test func sizesStepUpAndDownLikeTheWebControls() {
        #expect(CallSurface.Size.split.steppedUp == .expanded)
        #expect(CallSurface.Size.expanded.steppedUp == .immersive)
        #expect(CallSurface.Size.immersive.steppedUp == .expanded)
        #expect(CallSurface.Size.immersive.steppedDown == .expanded)
        #expect(CallSurface.Size.expanded.steppedDown == .split)
        #expect(CallSurface.Size.split.steppedDown == .split)
    }

    @Test func leavingFullscreenChangesOnlyImmersive() {
        #expect(CallSurface.Size.immersive.afterFullscreenExit == .expanded)
        #expect(CallSurface.Size.expanded.afterFullscreenExit == .expanded)
        #expect(CallSurface.Size.split.afterFullscreenExit == .split)
    }

    @Test func aFreshCallDefaultsToSplitAndOnlySplitAndExpandedRestore() {
        #expect(CallSurface.Size.defaultSize == .split)
        #expect(CallSurface.Size.restored(from: "split") == .split)
        #expect(CallSurface.Size.restored(from: "expanded") == .expanded)
        #expect(CallSurface.Size.restored(from: "immersive") == .split)
        #expect(CallSurface.Size.restored(from: "invalid") == .split)
        #expect(CallSurface.Size.restored(from: nil) == .split)
    }

    @Test(arguments: [
        (5.0, 25.0), (25.0, 25.0), (40.0, 40.0), (75.0, 75.0), (95.0, 75.0),
        (Double.nan, 50.0), (Double.infinity, 50.0),
    ])
    func splitLeavesBothPanesRoom(percent: Double, expected: Double) {
        #expect(CallSurface.defaultSplitPercent == 50)
        #expect(CallSurface.clampSplitPercent(percent) == expected)
    }

    // MARK: - Who draws the call

    @Test func theShellDockYieldsToAStage() {
        for surface in [CallSurface.split, .expanded, .fullscreen] {
            #expect(surface.showsStage)
            #expect(surface.dockContent(isInSession: true, isInSheet: false) == .hidden)
        }
        for surface in [CallSurface.dock, .sidebarCard] {
            #expect(!surface.showsStage)
            #expect(surface.dockContent(isInSession: true, isInSheet: false) == .call)
            #expect(surface.dockContent(isInSession: false, isInSheet: false) == .call)
        }
    }

    @Test(arguments: [CallSurface.dock, .sidebarCard, .split, .expanded, .fullscreen])
    func aSheetKeepsTheControlsButLeavesVideoToTheSurfaceBehindIt(surface: CallSurface) {
        #expect(surface.dockContent(isInSession: true, isInSheet: true) == .controls)
        // A ring must still be seen and answered over a sheet.
        #expect(surface.dockContent(isInSession: false, isInSheet: true) == .call)
    }

    @Test func theStageStandsBesideAConversationThatKeepsItsWidth() {
        #expect(CallSurface.splitArrangement(in: CGSize(width: 880, height: 700)) == .beside(width: 400))
        #expect(CallSurface.splitArrangement(in: CGSize(width: 1400, height: 700)) == .beside(width: 400))
    }

    @Test func theStageStacksAboveAConversationItWouldSqueeze() {
        #expect(CallSurface.splitArrangement(in: CGSize(width: 879, height: 700)) == .above(height: 350))
        #expect(CallSurface.splitArrangement(in: CGSize(width: 500, height: 0)) == .above(height: 0))
    }

    @Test func onlyAnEstablishedCallHasAConversationToStandBeside() {
        let join = CallJoin(url: "wss://sfu.waddle.test", room: "r", identity: "ada@waddle.test/mac", token: "t")
        let active = CallState.active(ActiveCall(peer: Self.peer, sid: "c1", media: Self.media, join: join))
        let inGroup = CallState.inGroup(GroupCall(room: Self.room, sid: "g1", media: Self.media, join: join, nick: "ada"))
        #expect(active.sessionConversation == .direct(Self.peer.bare))
        #expect(inGroup.sessionConversation == .room(Self.room))
        let others: [CallState] = [
            .idle, .incoming(IncomingCall(from: Self.peer, sid: "c1", media: Self.media)),
            .outgoing(OutgoingCall(to: Self.peer.bare, sid: "c1", media: Self.media)),
            .joiningGroup(GroupCallAttempt(room: Self.room, sid: "g1", media: Self.media, nick: "ada")),
            .ended(sid: "c1", reason: .timeout),
        ]
        for state in others {
            #expect(state.sessionConversation == nil)
            #expect(CallSurface.placement(of: state, open: .direct(Self.peer.bare), platform: .regularWidth) == .dock)
        }
        #expect(CallSurface.placement(of: active, open: .direct(Self.peer.bare), platform: .regularWidth) == .split)
        #expect(CallSurface.placement(of: active, open: .room(Self.room), platform: .regularWidth) == .sidebarCard)
        #expect(CallSurface.placement(of: active, open: nil, platform: .regularWidth) == .sidebarCard)
        #expect(CallSurface.placement(of: inGroup, open: .room(Self.room), platform: .regularWidth) == .split)
        #expect(CallSurface.placement(of: inGroup, open: .room(Self.room), platform: .phone) == .dock)
    }
}
