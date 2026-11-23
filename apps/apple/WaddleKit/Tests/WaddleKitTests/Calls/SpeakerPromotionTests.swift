import Testing

@testable import WaddleKit

@Suite("Speaker promotion") struct SpeakerPromotionTests {
    private static let bob = "bob@waddle.test/web"
    private static let carol = "carol@waddle.test/web"
    private static let dave = "dave@waddle.test/web"

    @Test func activeSpeakersAreRankedByLevelWithStableTies() {
        let local = "alice@waddle.test/mac"
        let active = SpeakerPromotion.activeLoudestFirst([
            (local, true, 0.4),
            (Self.bob, false, 1),
            (Self.carol, true, 0.8),
            (Self.dave, true, 0.8),
        ])
        #expect(active == [Self.carol, Self.dave, local])
        #expect(SpeakerPromotion.advance(nil, activeLoudestFirst: active) == Self.carol)
    }

    @Test(arguments: [
        (nil, [bob], bob),
        (bob, [carol, bob], bob),
        (bob, [], bob),
        (bob, [carol], carol),
        (bob, [dave, carol], dave),
        (nil, [], nil),
    ] as [(String?, [String], String?)])
    func holdsThroughCoSpeakersAndSilenceThenMovesToTheLoudest(
        current: String?, active: [String], expected: String?
    ) {
        #expect(SpeakerPromotion.advance(current, activeLoudestFirst: active) == expected)
    }
}
