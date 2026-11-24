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

    @Test func theShellDockYieldsToAStageAndToTheSidebarCard() {
        for surface in [CallSurface.split, .expanded, .fullscreen] {
            #expect(surface.showsStage)
            #expect(surface.dockContent(isInSession: true, isInSheet: false) == .hidden)
        }
        #expect(!CallSurface.sidebarCard.showsStage)
        #expect(CallSurface.sidebarCard.dockContent(isInSession: true, isInSheet: false) == .hidden)
        #expect(!CallSurface.dock.showsStage)
        #expect(CallSurface.dock.dockContent(isInSession: true, isInSheet: false) == .call)
        #expect(CallSurface.dock.dockContent(isInSession: false, isInSheet: false) == .call)
    }

    @Test func aCallAwayFromItsConversationNeedsTheSidebarForItsCard() {
        let join = CallJoin(url: "wss://sfu.waddle.test", room: "r", identity: "ada@waddle.test/mac", token: "t")
        let active = CallState.active(ActiveCall(peer: Self.peer, sid: "c1", media: Self.media, join: join))
        func surface(open: ConversationID?, isSidebarShown: Bool) -> CallSurface {
            CallSurface.placement(of: active, open: open, platform: .regularWidth, pane: nil, isSidebarShown: isSidebarShown)
        }
        #expect(surface(open: nil, isSidebarShown: true) == .sidebarCard)
        // With the sidebar hidden nothing else would show the call or its controls.
        #expect(surface(open: nil, isSidebarShown: false) == .dock)
        // The stage does not need the sidebar.
        #expect(surface(open: .direct(Self.peer.bare), isSidebarShown: false) == .split)
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
    }

    @Test func aPaneTooShortForAUsableStageHasNone() {
        let least = CallSurface.minimumStageHeight
        #expect(CallSurface.splitArrangement(in: CGSize(width: 1200, height: least)) == .beside(width: 400))
        #expect(CallSurface.splitArrangement(in: CGSize(width: 1200, height: least - 1)) == nil)
        // Above, the stage has half the pane.
        #expect(CallSurface.splitArrangement(in: CGSize(width: 600, height: 2 * least)) == .above(height: least))
        #expect(CallSurface.splitArrangement(in: CGSize(width: 600, height: 2 * least - 1)) == nil)
        #expect(CallSurface.splitArrangement(in: CGSize(width: 500, height: 0)) == nil)
    }

    @Test func aDraggedSplitGivesTheStageItsShareOfThePane() {
        // Beside: the share is of the width, never under the column and never squeezing the conversation.
        let wide = CGSize(width: 1400, height: 700)
        #expect(CallSurface.splitArrangement(in: wide, percent: 50) == .beside(width: 700))
        #expect(CallSurface.splitArrangement(in: wide, percent: 25) == .beside(width: 400))
        #expect(CallSurface.splitArrangement(in: wide, percent: 75) == .beside(width: 920))
        #expect(CallSurface.splitArrangement(in: wide, percent: 900) == .beside(width: 920))
        // Above: the share is of the height, never under a usable stage.
        let narrow = CGSize(width: 600, height: 1000)
        #expect(CallSurface.splitArrangement(in: narrow, percent: 60) == .above(height: 600))
        #expect(CallSurface.splitArrangement(in: narrow, percent: 25) == .above(height: CallSurface.minimumStageHeight))
        #expect(CallSurface.splitArrangement(in: narrow, percent: 75) == .above(height: 750))
    }

    @Test func aDragNeverCostsAPaneItsStage() {
        let least = CallSurface.minimumStageHeight
        #expect(CallSurface.splitArrangement(in: CGSize(width: 600, height: 2 * least), percent: 25) == .above(height: least))
        #expect(CallSurface.splitArrangement(in: CGSize(width: 600, height: 2 * least - 1), percent: 75) == nil)
    }

    @Test func aDragIsReadAsAShareOfThePane() {
        #expect(CallSurface.splitPercent(stage: 300, of: 1000) == 30)
        #expect(CallSurface.splitPercent(stage: 100, of: 1000) == 25)
        #expect(CallSurface.splitPercent(stage: 990, of: 1000) == 75)
        #expect(CallSurface.splitPercent(stage: 10, of: 0) == CallSurface.defaultSplitPercent)
    }

    @Test(arguments: [CallSurface.Size.expanded, .immersive])
    func aLargerStageFillsItsPane(size: CallSurface.Size) {
        let least = CallSurface.minimumStageHeight
        #expect(CallSurface.stageArrangement(for: size, in: CGSize(width: 500, height: least), splitPercent: 30) == .filling)
        #expect(CallSurface.stageArrangement(for: size, in: CGSize(width: 1400, height: least - 1), splitPercent: nil) == nil)
    }

    @Test func aSplitStageIsArrangedByTheSplit() {
        let pane = CGSize(width: 1400, height: 700)
        #expect(CallSurface.stageArrangement(for: .split, in: pane, splitPercent: nil) == .beside(width: 400))
        #expect(CallSurface.stageArrangement(for: .split, in: pane, splitPercent: 50) == .beside(width: 700))
    }

    @Test func anExpandedCallNeedsOnlyTheHeightOfAStage() {
        let join = CallJoin(url: "wss://sfu.waddle.test", room: "r", identity: "ada@waddle.test/mac", token: "t")
        let state = CallState.active(ActiveCall(peer: Self.peer, sid: "c1", media: Self.media, join: join))
        // Too short to share with a conversation above, tall enough alone.
        let pane = CGSize(width: 600, height: CallSurface.minimumStageHeight)
        let open = ConversationID.direct(Self.peer.bare)
        #expect(CallSurface.placement(of: state, open: open, platform: .regularWidth, pane: pane) == .dock)
        #expect(CallSurface.placement(of: state, open: open, platform: .regularWidth, pane: pane, size: .expanded) == .expanded)
        #expect(CallSurface.placement(of: state, open: open, platform: .regularWidth, pane: pane, size: .immersive) == .fullscreen)
    }

    @Test func aCallWithNoRoomForAStageStaysInTheDock() {
        let join = CallJoin(url: "wss://sfu.waddle.test", room: "r", identity: "ada@waddle.test/mac", token: "t")
        let active = CallState.active(ActiveCall(peer: Self.peer, sid: "c1", media: Self.media, join: join))
        let open = ConversationID.direct(Self.peer.bare)
        func surface(_ pane: CGSize?) -> CallSurface {
            CallSurface.placement(of: active, open: open, platform: .regularWidth, pane: pane)
        }
        // Before the pane is laid out, assume it has room.
        #expect(surface(nil) == .split)
        #expect(surface(CGSize(width: 1000, height: 700)) == .split)
        #expect(surface(CGSize(width: 900, height: 200)) == .dock)
        #expect(surface(CGSize(width: 700, height: 400)) == .dock)
        #expect(surface(CGSize(width: 700, height: 400)).dockContent(isInSession: true, isInSheet: false) == .call)
    }

    @Test func aPhoneIsAPhoneHoweverItIsHeld() {
        typealias Platform = CallSurface.Platform
        // A large iPhone in landscape is regular width, and still no place for a stage.
        #expect(Platform(isPhone: true, isCompactWidth: false, isCompactHeight: true) == .phone)
        #expect(Platform(isPhone: true, isCompactWidth: false, isCompactHeight: false) == .phone)
        #expect(Platform(isPhone: true, isCompactWidth: true, isCompactHeight: false) == .phone)
        // An iPad sharing the screen, or any window squeezed flat.
        #expect(Platform(isPhone: false, isCompactWidth: true, isCompactHeight: false) == .phone)
        #expect(Platform(isPhone: false, isCompactWidth: false, isCompactHeight: true) == .phone)
        #expect(Platform(isPhone: false, isCompactWidth: false, isCompactHeight: false) == .regularWidth)
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
            #expect(CallSurface.placement(of: state, open: .direct(Self.peer.bare), platform: .regularWidth, pane: nil) == .dock)
        }
        #expect(CallSurface.placement(of: active, open: .direct(Self.peer.bare), platform: .regularWidth, pane: nil) == .split)
        #expect(CallSurface.placement(of: active, open: .room(Self.room), platform: .regularWidth, pane: nil) == .sidebarCard)
        #expect(CallSurface.placement(of: active, open: nil, platform: .regularWidth, pane: nil) == .sidebarCard)
        #expect(CallSurface.placement(of: inGroup, open: .room(Self.room), platform: .regularWidth, pane: nil) == .split)
        #expect(CallSurface.placement(of: inGroup, open: .room(Self.room), platform: .phone, pane: nil) == .dock)
    }
}
