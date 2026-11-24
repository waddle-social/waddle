import Testing

@testable import WaddleKit

@Suite("Call stage memory") struct CallStageMemoryTests {
    private static let own = CallStageTile(identity: "ada@waddle.test/mac", source: .camera, label: "You", isSelf: true)
    private static let linus = CallStageTile(identity: "linus@waddle.test/web", source: .camera, label: "Linus")
    private static let grace = CallStageTile(identity: "grace@waddle.test/web", source: .camera, label: "Grace")

    @Test func aNewCallStartsFromTheDefaults() {
        var memory = CallStageMemory()
        memory.follow(call: "c1")
        memory.presentation.viewMode = .speaker
        memory.togglePin(Self.linus.id)
        memory.speakersChanged([Self.linus.identity])

        memory.follow(call: "c1")
        #expect(memory.presentation.viewMode == .speaker)
        #expect(memory.presentation.pinnedTileID == Self.linus.id)
        #expect(memory.promotedSpeaker == Self.linus.identity)

        memory.follow(call: "c2")
        #expect(memory.presentation == CallStagePresentation())
        #expect(memory.promotedSpeaker == nil)
    }

    @Test func pinningThePinnedTileUnpinsIt() {
        var memory = CallStageMemory()
        memory.togglePin(Self.linus.id)
        memory.togglePin(Self.grace.id)
        #expect(memory.presentation.pinnedTileID == Self.grace.id)
        memory.togglePin(Self.grace.id)
        #expect(memory.presentation.pinnedTileID == nil)
    }

    @Test func aPinGoesWithItsTileButSurvivesAReconnect() {
        var memory = CallStageMemory()
        memory.togglePin(Self.linus.id)

        // The roster is gone while media reconnects: whatever stands in for it is not the truth.
        memory.tilesChanged([Self.own], rosterIsAvailable: false)
        #expect(memory.presentation.pinnedTileID == Self.linus.id)

        memory.tilesChanged([Self.own, Self.grace], rosterIsAvailable: true)
        #expect(memory.presentation.pinnedTileID == nil)
    }

    @Test func hidingOurOwnViewUnpinsIt() {
        var memory = CallStageMemory()
        memory.togglePin(Self.own.id)
        memory.presentation.isSelfViewHidden = true
        memory.tilesChanged([Self.own, Self.linus], rosterIsAvailable: true)
        #expect(memory.presentation.pinnedTileID == nil)
    }

    @Test func thePromotedSpeakerHoldsThroughSilence() {
        var memory = CallStageMemory()
        memory.speakersChanged([Self.linus.identity])
        memory.speakersChanged([])
        #expect(memory.promotedSpeaker == Self.linus.identity)
        memory.speakersChanged([Self.grace.identity])
        #expect(memory.promotedSpeaker == Self.grace.identity)
    }

    @Test func whatChangesNothingLeavesTheMemoryEqual() {
        var memory = CallStageMemory()
        memory.follow(call: "c1")
        memory.speakersChanged([Self.linus.identity])
        memory.tilesChanged([Self.own, Self.linus], rosterIsAvailable: true)
        let before = memory

        memory.follow(call: "c1")
        memory.speakersChanged([Self.linus.identity, Self.grace.identity])
        memory.speakersChanged([])
        memory.tilesChanged([Self.own, Self.linus], rosterIsAvailable: true)
        #expect(memory == before)
    }

    @Test func aStageKeepsItsSizeForItsCallOnly() {
        var memory = CallStageMemory()
        memory.follow(call: "a")
        #expect(memory.size == .split)
        memory.size = memory.size.steppedUp
        memory.follow(call: "a")
        #expect(memory.size == .expanded)
        memory.follow(call: "b")
        #expect(memory.size == .split)
    }

    @Test func theParticipantsPanelExpandsASplitStageAndClosesWithIt() {
        var memory = CallStageMemory()
        memory.follow(call: "a")
        memory.toggleParticipants()
        #expect(memory.showsParticipants)
        #expect(memory.size == .expanded)
        memory.size = .immersive
        #expect(memory.showsParticipants)
        memory.toggleParticipants()
        #expect(!memory.showsParticipants)
        #expect(memory.size == .immersive)
        memory.toggleParticipants()
        memory.size = .split
        #expect(!memory.showsParticipants)
        memory.toggleParticipants()
        memory.follow(call: "b")
        #expect(!memory.showsParticipants)
    }
}
