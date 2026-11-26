import Testing

@testable import WaddleKit

/// Whose picture a floating call shows (web `selectCallPictureInPictureTile`).
struct CallPictureInPictureTests {
    private static func camera(_ name: String, isSelf: Bool = false, hasVideo: Bool = true) -> CallStageTile {
        CallStageTile(identity: "\(name)@waddle.test/web", source: .camera, label: name, isSelf: isSelf, hasVideo: hasVideo)
    }

    private static let own = camera("ada", isSelf: true)
    private static let linus = camera("linus")
    private static let grace = camera("grace")
    private static let screen = CallStageTile(
        identity: linus.identity, source: .screen, label: "linus", hasVideo: true, screenPublicationID: "s1"
    )

    private func tile(
        _ tiles: [CallStageTile], _ presentation: CallStagePresentation = .init(), speaker: String? = nil
    ) -> CallStageTile? {
        CallPictureInPicture.tile(in: tiles, presentation: presentation, speaker: speaker)
    }

    @Test func nobodyWithAPictureLeavesNothingToFloat() {
        #expect(tile([Self.own, Self.linus].map { Self.camera($0.label, isSelf: $0.isSelf, hasVideo: false) }) == nil)
    }

    @Test func someoneElsesCameraComesBeforeOurOwn() {
        #expect(tile([Self.own, Self.linus, Self.grace]) == Self.linus)
        #expect(tile([Self.own]) == Self.own)
    }

    @Test func theSpeakerComesBeforeTheFirstInTheList() {
        #expect(tile([Self.own, Self.linus, Self.grace], speaker: Self.grace.identity) == Self.grace)
    }

    @Test func aSpeakerWithNoPictureIsPassedOver() {
        let silentPicture = Self.camera("grace", hasVideo: false)
        #expect(tile([Self.own, Self.linus, silentPicture], speaker: silentPicture.identity) == Self.linus)
    }

    @Test func aPinComesBeforeASpeaker() {
        let pinned = CallStagePresentation(pinnedTileID: Self.screen.id)
        #expect(tile([Self.own, Self.linus, Self.screen, Self.grace], pinned, speaker: Self.grace.identity) == Self.screen)
    }

    @Test func aScreenFloatsOnlyWhenNoCameraHasAPicture() {
        let dark = Self.camera("linus", hasVideo: false)
        #expect(tile([dark, Self.screen]) == Self.screen)
        #expect(tile([Self.linus, Self.screen]) == Self.linus)
    }

    @Test func ourOwnHiddenViewDoesNotFloat() {
        #expect(tile([Self.own], CallStagePresentation(isSelfViewHidden: true)) == nil)
    }
}
