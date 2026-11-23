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
}
