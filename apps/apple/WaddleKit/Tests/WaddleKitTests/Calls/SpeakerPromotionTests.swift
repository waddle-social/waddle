import Testing

@testable import WaddleKit

@Suite("Speaker promotion") struct SpeakerPromotionTests {
    private static let bob = "bob@waddle.test/web"
    private static let carol = "carol@waddle.test/web"
    private static let dave = "dave@waddle.test/web"

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
