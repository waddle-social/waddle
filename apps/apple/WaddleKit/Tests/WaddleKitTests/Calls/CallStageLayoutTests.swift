import Foundation
import Testing

@testable import WaddleKit

@Suite("Call stage layout") struct CallStageLayoutTests {
    private let large = CGSize(width: 1600, height: 1100)
    private let own = camera("alice", isSelf: true)
    private let bob = camera("bob")
    private let carol = camera("carol")

    @Test func cameraAndScreenHaveDistinctTileIDsButShareTheRosterAccount() {
        let camera = CallStageTile(identity: "BOB@waddle.test/phone", source: .camera, label: "Bobby")
        let screen = CallStageTile(identity: "BOB@waddle.test/phone", source: .screen, label: "Bobby")
        #expect(camera.id != screen.id)
        #expect(camera.bareJID == BareJID(parsing: "bob@waddle.test"))
        #expect(screen.bareJID == camera.bareJID)
    }

    @Test func galleryPutsOurCameraFirstAndKeepsRemoteArrivalOrder() {
        let presentation = CallStagePresentation()
        let layout = CallStageLayout.make(
            tiles: [carol, own, bob], presentation: presentation, promoted: bob.identity, size: large
        )
        #expect(layout == .grid(columns: 2, rows: 2, tiles: [own, carol, bob], hiddenCount: 0))
    }

    @Test func personBadgesBelongToCameraTiles() {
        let camera = CallStageTile(
            identity: bob.identity, source: .camera, label: "Bobby", isMuted: true,
            isHandRaised: true, isSpeaking: true, hasVideo: false
        )
        let screen = CallStageTile(
            identity: bob.identity, source: .screen, label: "Bobby’s screen", isMuted: true,
            isHandRaised: true, isSpeaking: true, hasVideo: true
        )

        #expect(camera.label == "Bobby")
        #expect(camera.isMuted && camera.isHandRaised && camera.isSpeaking)
        #expect(!camera.hasVideo)
        #expect(screen.label == "Bobby’s screen")
        #expect(!screen.isMuted && !screen.isHandRaised && !screen.isSpeaking)
        #expect(screen.hasVideo)
    }

    @Test func hidingSelfViewRemovesOnlyOurCamera() {
        let share = CallStageTile(identity: own.identity, source: .screen, label: "Screen", isSelf: true, hasVideo: true)
        let presentation = CallStagePresentation(isSelfViewHidden: true)

        #expect(CallStageLayout.make(tiles: [own, share, bob], presentation: presentation, size: large)
            == .grid(columns: 2, rows: 1, tiles: [share, bob], hiddenCount: 0))
    }

    @Test func honorsPinAndKeepsTheOtherTilesInOrder() {
        let presentation = CallStagePresentation(pinnedTileID: carol.id)

        #expect(CallStageLayout.make(tiles: [own, bob, carol], presentation: presentation, size: large)
            == .spotlight(main: carol, strip: [own, bob]))
        #expect(presentation.pinnedTileID == carol.id)
    }

    @Test func layoutKeepsThePinThroughAnUnavailableRoster() {
        let presentation = CallStagePresentation(pinnedTileID: bob.id)
        #expect(CallStageLayout.make(tiles: [], presentation: presentation, size: large)
            == .grid(columns: 1, rows: 1, tiles: [], hiddenCount: 0))
        #expect(presentation.pinnedTileID == bob.id)
        #expect(CallStageLayout.make(tiles: [own, bob], presentation: presentation, size: large)
            == .spotlight(main: bob, strip: [own]))
    }

    @Test func clearsPinWhenTheTileLeavesSoItStaysClearedWhenTheyRejoin() {
        var presentation = CallStagePresentation(pinnedTileID: bob.id)
        presentation.reconcile(with: [own, carol])
        #expect(CallStageLayout.make(tiles: [own, carol], presentation: presentation, size: large)
            == .grid(columns: 2, rows: 1, tiles: [own, carol], hiddenCount: 0))
        #expect(presentation.pinnedTileID == nil)
        presentation.reconcile(with: [own, carol, bob])
        #expect(CallStageLayout.make(tiles: [own, carol, bob], presentation: presentation, size: large)
            == .grid(columns: 2, rows: 2, tiles: [own, carol, bob], hiddenCount: 0))
    }

    @Test func hidingSelfViewClearsItsPin() {
        var presentation = CallStagePresentation(pinnedTileID: own.id, isSelfViewHidden: true)
        presentation.reconcile(with: [own, bob])
        #expect(CallStageLayout.make(tiles: [own, bob], presentation: presentation, size: large)
            == .grid(columns: 1, rows: 1, tiles: [bob], hiddenCount: 0))
        #expect(presentation.pinnedTileID == nil)
        presentation.isSelfViewHidden = false
        #expect(CallStageLayout.make(tiles: [own, bob], presentation: presentation, size: large)
            == .grid(columns: 2, rows: 1, tiles: [own, bob], hiddenCount: 0))
    }

    @Test func speakerViewPromotesTheSpeakerAndPinTakesPrecedence() {
        var presentation = CallStagePresentation(viewMode: .speaker)
        #expect(CallStageLayout.make(
            tiles: [own, bob, carol], presentation: presentation, promoted: carol.identity, size: large
        ) == .spotlight(main: carol, strip: [own, bob]))

        presentation.pinnedTileID = bob.id
        #expect(CallStageLayout.make(
            tiles: [own, bob, carol], presentation: presentation, promoted: carol.identity, size: large
        ) == .spotlight(main: bob, strip: [own, carol]))
    }

    @Test func speakerViewFallsBackToTheFirstRemoteCameraThenOurCamera() {
        let presentation = CallStagePresentation(viewMode: .speaker)
        let neverSpoken: String? = nil
        let departed: String? = "departed@waddle.test/web"
        for promoted in [neverSpoken, departed] {
            #expect(CallStageLayout.make(
                tiles: [own, carol, bob], presentation: presentation, promoted: promoted, size: large
            ) == .spotlight(main: carol, strip: [own, bob]))
        }
        #expect(CallStageLayout.make(tiles: [own], presentation: presentation, size: large)
            == .spotlight(main: own, strip: []))
    }

    @Test func speakerViewNeverPromotesTheHiddenSelfView() {
        let presentation = CallStagePresentation(viewMode: .speaker, isSelfViewHidden: true)
        #expect(CallStageLayout.make(
            tiles: [own, bob], presentation: presentation, promoted: own.identity, size: large
        ) == .spotlight(main: bob, strip: []))
        #expect(CallStageLayout.make(
            tiles: [own], presentation: presentation, promoted: own.identity, size: large
        ) == .grid(columns: 1, rows: 1, tiles: [], hiddenCount: 0))
    }

    @Test func aRemoteScreenOutranksBothPinAndSpeakerUntilTheShareEnds() {
        let share = CallStageTile(identity: bob.identity, source: .screen, label: "Bob’s screen", hasVideo: true)
        var presentation = CallStagePresentation(viewMode: .speaker, pinnedTileID: carol.id)
        presentation.reconcile(with: [own, bob, share, carol])
        #expect(CallStageLayout.make(
            tiles: [own, bob, share, carol], presentation: presentation, promoted: bob.identity, size: large
        ) == .spotlight(main: share, strip: [own, bob, carol]))
        #expect(presentation.pinnedTileID == carol.id)
        presentation.reconcile(with: [own, bob, carol])
        #expect(CallStageLayout.make(
            tiles: [own, bob, carol], presentation: presentation, promoted: bob.identity, size: large
        ) == .spotlight(main: carol, strip: [own, bob]))
    }

    @Test func aRemoteScreenIsTheOnlyKindOfScreenThatAutomaticallyTakesSpotlight() {
        let selfShare = CallStageTile(identity: own.identity, source: .screen, label: "Screen", isSelf: true, hasVideo: true)
        let emptyShare = CallStageTile(identity: bob.identity, source: .screen, label: "Bob’s screen")
        let tiles = [own, selfShare, emptyShare]
        var presentation = CallStagePresentation()
        presentation.reconcile(with: tiles)
        #expect(CallStageLayout.make(tiles: tiles, presentation: presentation, size: large)
            == .grid(columns: 2, rows: 2, tiles: [own, selfShare, emptyShare], hiddenCount: 0))
    }

    @Test func twoScreensArrivingTogetherFeatureTheLastOneOnEveryLayout() {
        let first = CallStageTile(identity: bob.identity, source: .screen, label: "Bob’s screen", hasVideo: true)
        let last = CallStageTile(identity: carol.identity, source: .screen, label: "Carol’s screen", hasVideo: true)
        let tiles = [own, first, last]
        var presentation = CallStagePresentation()

        presentation.reconcile(with: tiles)
        for _ in 0..<2 {
            #expect(CallStageLayout.make(tiles: tiles, presentation: presentation, size: large)
                == .spotlight(main: last, strip: [own, first]))
        }
    }

    @Test func newestRemoteScreenStaysOnStageWhenItsParticipantIsEarlierInArrivalOrder() {
        let bobShare = CallStageTile(identity: bob.identity, source: .screen, label: "Bob’s screen", hasVideo: true)
        let carolShare = CallStageTile(identity: carol.identity, source: .screen, label: "Carol’s screen", hasVideo: true)
        var presentation = CallStagePresentation()
        presentation.reconcile(with: [own, carolShare])
        #expect(CallStageLayout.make(tiles: [own, carolShare], presentation: presentation, size: large)
            == .spotlight(main: carolShare, strip: [own]))
        presentation.reconcile(with: [own, bobShare, carolShare])
        for _ in 0..<2 {
            #expect(CallStageLayout.make(tiles: [own, bobShare, carolShare], presentation: presentation, size: large)
                == .spotlight(main: bobShare, strip: [own, carolShare]))
        }
        presentation.reconcile(with: [own, carolShare])
        #expect(CallStageLayout.make(tiles: [own, carolShare], presentation: presentation, size: large)
            == .spotlight(main: carolShare, strip: [own]))
        presentation.reconcile(with: [own])
        #expect(CallStageLayout.make(tiles: [own], presentation: presentation, size: large)
            == .grid(columns: 1, rows: 1, tiles: [own], hiddenCount: 0))
    }

    @Test func restartedRemoteScreenIsNewEvenWithTheSameTileID() {
        let bobFirst = CallStageTile(
            identity: bob.identity, source: .screen, label: "Bob’s screen", hasVideo: true, screenPublicationID: "bob-1"
        )
        let bobRestarted = CallStageTile(
            identity: bob.identity, source: .screen, label: "Bob’s screen", hasVideo: true, screenPublicationID: "bob-2"
        )
        let carolShare = CallStageTile(
            identity: carol.identity, source: .screen, label: "Carol’s screen", hasVideo: true,
            screenPublicationID: "carol-1"
        )
        var presentation = CallStagePresentation()
        #expect(bobFirst.id == bobRestarted.id)
        presentation.reconcile(with: [bobFirst])
        #expect(CallStageLayout.make(tiles: [bobFirst], presentation: presentation, size: large)
            == .spotlight(main: bobFirst, strip: []))
        presentation.reconcile(with: [bobFirst, carolShare])
        #expect(CallStageLayout.make(tiles: [bobFirst, carolShare], presentation: presentation, size: large)
            == .spotlight(main: carolShare, strip: [bobFirst]))
        presentation.reconcile(with: [carolShare, bobRestarted])
        #expect(CallStageLayout.make(tiles: [carolShare, bobRestarted], presentation: presentation, size: large)
            == .spotlight(main: bobRestarted, strip: [carolShare]))
        #expect(CallStageLayout.make(tiles: [carolShare, bobRestarted], presentation: presentation, size: large)
            == .spotlight(main: bobRestarted, strip: [carolShare]))
    }

    @Test func overflowReservesTheLastCellAndShowsEveryoneWhenTheyFit() {
        let presentation = CallStagePresentation()
        let tiles = (0..<30).map { camera("p\($0)") }
        #expect(CallStageLayout.make(tiles: tiles, presentation: presentation, size: large)
            == .grid(columns: 5, rows: 5, tiles: Array(tiles.prefix(24)), hiddenCount: 6))

        let exactlyFull = Array(tiles.prefix(25))
        #expect(CallStageLayout.make(tiles: exactlyFull, presentation: presentation, size: large)
            == .grid(columns: 5, rows: 5, tiles: exactlyFull, hiddenCount: 0))

        let belowCapacity = Array(tiles.prefix(7))
        #expect(CallStageLayout.make(tiles: belowCapacity, presentation: presentation, size: large)
            == .grid(columns: 3, rows: 3, tiles: belowCapacity, hiddenCount: 0))
    }

    @Test func overflowCountsPeopleWhoHaveNoTileOnStage() {
        let presentation = CallStagePresentation()
        let bobScreen = CallStageTile(identity: bob.identity, source: .screen, label: "Bob’s screen")
        let ownScreen = CallStageTile(identity: own.identity, source: .screen, label: "Screen", isSelf: true)
        #expect(CallStageLayout.make(
            tiles: [own, bob, carol, bobScreen], presentation: presentation, size: large, maxTiles: 3
        ) == .grid(columns: 2, rows: 2, tiles: [own, bob], hiddenCount: 1))
        #expect(CallStageLayout.make(
            tiles: [own, bob, ownScreen, bobScreen], presentation: presentation, size: large, maxTiles: 3
        ) == .grid(columns: 2, rows: 2, tiles: [own, bob, ownScreen], hiddenCount: 0))
    }

    @Test func overflowHonorsTheStageSizeAndTheCellCap() {
        let presentation = CallStagePresentation()
        let tiles = (0..<9).map { camera("p\($0)") }
        #expect(CallStageLayout.make(
            tiles: tiles, presentation: presentation, size: CGSize(width: 600, height: 400), maxTiles: 5
        ) == .grid(columns: 2, rows: 2, tiles: Array(tiles.prefix(3)), hiddenCount: 6))
        #expect(CallStageLayout.make(
            tiles: tiles, presentation: presentation, size: large, maxTiles: 2
        ) == .grid(columns: 2, rows: 1, tiles: Array(tiles.prefix(1)), hiddenCount: 8))
    }

    @Test func overflowLeavesTheOnlyCellForAVideoTile() {
        let presentation = CallStagePresentation()
        let tiles = [own, bob, carol]
        #expect(CallStageLayout.make(
            tiles: tiles, presentation: presentation, size: CGSize(width: 240, height: 180)
        ) == .grid(columns: 1, rows: 1, tiles: [own], hiddenCount: 0))
        for maxTiles in [0, 1] {
            #expect(CallStageLayout.make(tiles: tiles, presentation: presentation, size: large, maxTiles: maxTiles)
                == .grid(columns: 1, rows: 1, tiles: [own], hiddenCount: 0))
        }
    }
}

private func camera(_ name: String, isSelf: Bool = false) -> CallStageTile {
    CallStageTile(identity: "\(name)@waddle.test/web", source: .camera, label: name, isSelf: isSelf)
}
