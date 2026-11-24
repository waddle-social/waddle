import Foundation
import Testing

@testable import WaddleKit

@Suite("Call stage tiles") struct CallStageTilesTests {
    private static let own = JID(parsing: "ada@waddle.test/mac")!
    private static let peer = JID(parsing: "linus@waddle.test/web")!
    private static let grace = JID(parsing: "grace@waddle.test/phone")!

    private static func live(_ jid: JID, isSelf: Bool = false, hasCamera: Bool = false) -> CallStageParticipant {
        CallStageParticipant(identity: jid.description, isSelf: isSelf, hasCamera: hasCamera)
    }

    // MARK: - 1:1

    @Test func aDirectCallShowsBothPeopleBeforeMediaConnects() {
        let tiles = CallStageTiles.direct(live: [], own: Self.own, peer: Self.peer, peerName: "Linus")
        #expect(tiles == [
            CallStageTile(identity: Self.own.description, source: .camera, label: "You", isSelf: true),
            CallStageTile(identity: Self.peer.description, source: .camera, label: "Linus"),
        ])
    }

    @Test func aDirectCallKeepsThePeersPlaceUntilTheirDeviceJoins() {
        let tiles = CallStageTiles.direct(
            live: [Self.live(Self.own, isSelf: true, hasCamera: true)], own: Self.own, peer: Self.peer, peerName: "Linus"
        )
        #expect(tiles.map(\.identity) == [Self.own.description, Self.peer.description])
        #expect(tiles.map(\.hasVideo) == [true, false])
    }

    @Test func aDirectCallTakesTheLiveDeviceOfThePeerOverThePlaceholder() {
        let phone = JID(parsing: "linus@waddle.test/phone")!
        let tiles = CallStageTiles.direct(
            live: [Self.live(Self.own, isSelf: true), Self.live(phone, hasCamera: true)],
            own: Self.own, peer: Self.peer, peerName: "Linus", isSelfMuted: true,
            speaking: [phone.bare]
        )
        #expect(tiles == [
            CallStageTile(identity: Self.own.description, source: .camera, label: "You", isSelf: true, isMuted: true),
            CallStageTile(identity: phone.description, source: .camera, label: "Linus", isSpeaking: true, hasVideo: true),
        ])
    }

    // MARK: - Rooms

    private static let rows = [
        GroupCallRosterRow(nick: "ada", jid: own.bare, isMuted: true, isSelf: true),
        GroupCallRosterRow(nick: "Linus T", jid: peer.bare, isHandRaised: true, isMuted: true, isSpeaking: true),
        GroupCallRosterRow(nick: "guest", jid: nil),
    ]

    @Test func aRoomShowsItsRosterBeforeMediaConnects() {
        let tiles = CallStageTiles.room(live: [], own: Self.own, rows: Self.rows)
        #expect(tiles == [
            CallStageTile(identity: Self.own.description, source: .camera, label: "You", isSelf: true),
            CallStageTile(
                identity: Self.peer.bare.description, source: .camera, label: "Linus T", isMuted: true,
                isHandRaised: true, isSpeaking: true
            ),
            CallStageTile(identity: "guest", source: .camera, label: "guest"),
        ])
    }

    @Test func aRoomTileTakesItsNameAndBadgesFromTheRosterRowOfItsAccount() {
        let tiles = CallStageTiles.room(
            live: [Self.live(Self.own, isSelf: true), Self.live(Self.peer, hasCamera: true), Self.live(Self.grace)],
            own: Self.own, rows: Self.rows, isSelfMuted: false
        )
        #expect(tiles == [
            // Our own microphone is the truth for our tile, not what the room was last told.
            CallStageTile(identity: Self.own.description, source: .camera, label: "You", isSelf: true),
            CallStageTile(
                identity: Self.peer.description, source: .camera, label: "Linus T", isMuted: true,
                isHandRaised: true, isSpeaking: true, hasVideo: true
            ),
            // Nobody in the roster: the account's name, no badges.
            CallStageTile(identity: Self.grace.description, source: .camera, label: "grace"),
        ])
    }

    @Test func twoDevicesOfOneAccountAreTwoTilesWithOneName() {
        let phone = JID(parsing: "linus@waddle.test/phone")!
        let tiles = CallStageTiles.room(live: [Self.live(Self.peer), Self.live(phone)], own: Self.own, rows: Self.rows)
        #expect(tiles.map(\.label) == ["Linus T", "Linus T"])
        #expect(Set(tiles.map(\.id)).count == 2)
    }

    // MARK: - Words

    // MARK: - Shared screens

    @Test func aSharedScreenIsATileOfItsOwnAfterItsPersonsCamera() {
        let sharing = CallStageParticipant(
            identity: Self.peer.description, isSelf: false, hasCamera: true, screenPublicationID: "TR_screen1"
        )
        let tiles = CallStageTiles.direct(
            live: [Self.live(Self.own, isSelf: true), sharing], own: Self.own, peer: Self.peer, peerName: "Linus"
        )
        #expect(tiles.map(\.id) == [
            CallStageTile.ID(identity: Self.own.description, source: .camera),
            CallStageTile.ID(identity: Self.peer.description, source: .camera),
            CallStageTile.ID(identity: Self.peer.description, source: .screen),
        ])
        #expect(tiles[2] == CallStageTile(
            identity: Self.peer.description, source: .screen, label: "Linus's screen", hasVideo: true,
            screenPublicationID: "TR_screen1"
        ))
    }

    @Test func aScreenInARoomIsNamedForItsPersonAndCarriesNoneOfTheirBadges() {
        let rows = [GroupCallRosterRow(
            nick: "Grace H", jid: Self.grace.bare, isHandRaised: true, isMuted: true, isSpeaking: true, isSelf: false
        )]
        let sharing = CallStageParticipant(
            identity: Self.grace.description, isSelf: false, hasCamera: false, screenPublicationID: "TR_s"
        )
        let tiles = CallStageTiles.room(live: [Self.live(Self.own, isSelf: true), sharing], own: Self.own, rows: rows)
        let screen = tiles[2]
        #expect(screen.label == "Grace H's screen")
        // A mute, a hand and speaking belong to the person, whose own tile shows them.
        #expect(!screen.isMuted && !screen.isHandRaised && !screen.isSpeaking)
        #expect(tiles[1].isMuted && tiles[1].isHandRaised)
    }

    @Test func ourOwnShareIsCalledOurScreen() {
        let me = CallStageParticipant(identity: Self.own.description, isSelf: true, hasCamera: true, screenPublicationID: "TR_me")
        let tiles = CallStageTiles.direct(live: [me], own: Self.own, peer: Self.peer, peerName: "Linus")
        #expect(tiles.first { $0.source == .screen }?.label == "Your screen")
        #expect(tiles.first { $0.source == .screen }?.isSelf == true)
    }

    @Test func aRemoteScreenTakesTheLargeTile() {
        let sharing = CallStageParticipant(
            identity: Self.peer.description, isSelf: false, hasCamera: true, screenPublicationID: "TR_screen1"
        )
        let tiles = CallStageTiles.direct(
            live: [Self.live(Self.own, isSelf: true), sharing], own: Self.own, peer: Self.peer, peerName: "Linus"
        )
        var presentation = CallStagePresentation()
        presentation.reconcile(with: tiles)
        guard case let .spotlight(main, strip) = CallStageLayout.make(
            tiles: tiles, presentation: presentation, promoted: nil, size: CGSize(width: 900, height: 600), maxTiles: 9
        ) else {
            Issue.record("a shared screen is shown large even in gallery view")
            return
        }
        #expect(main.source == .screen)
        #expect(strip.map(\.source) == [.camera, .camera])
    }

    @Test func aTileIsSpokenWithItsBadges() {
        let plain = CallStageTile(identity: Self.peer.description, source: .camera, label: "Linus")
        let busy = CallStageTile(
            identity: Self.peer.description, source: .camera, label: "Linus", isMuted: true, isHandRaised: true,
            isSpeaking: true
        )
        #expect(plain.spokenLabel(isPinned: false) == "Linus")
        #expect(busy.spokenLabel(isPinned: false) == "Linus, muted, hand raised, speaking")
        #expect(busy.spokenLabel(isPinned: true) == "Linus, muted, hand raised, speaking, pinned")
    }

    @Test func theSubtitleNamesTheKindOfDirectCall() {
        #expect(CallStageSubtitle.direct(CallMedia(audio: true, video: true)) == "Video call")
        #expect(CallStageSubtitle.direct(CallMedia(audio: true, video: false)) == "Voice call")
    }

    @Test func theSubtitleCountsTheOthersInARoom() {
        #expect(CallStageSubtitle.room(others: 0) == "Waiting for others to join…")
        #expect(CallStageSubtitle.room(others: 1) == "1 other in call")
        #expect(CallStageSubtitle.room(others: 4) == "4 others in call")
    }
}
