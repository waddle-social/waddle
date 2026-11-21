import Foundation
import Testing
@testable import WaddleKit

private func story(_ id: String, video: Bool = false) -> Story {
    Story(
        id: id,
        media: URL(string: "https://upload.waddle.test/\(id).\(video ? "mp4" : "jpg")")!,
        mediaType: video ? "video/mp4" : "image/jpeg"
    )
}

private let up = [story("a"), story("b", video: true), story("c")]

@Suite("Stepping through stories")
struct StoryPlaythroughTests {
    @Test func opensAtTheStoryTappedAmongThoseUp() {
        let playthrough = StoryPlaythrough(start: story("b", video: true), among: up)

        #expect(playthrough.stories.map(\.id) == ["a", "b", "c"])
        #expect(playthrough.current.id == "b")
        #expect(!playthrough.isLast)
    }

    @Test func aStoryNoLongerUpOpensAlone() {
        let playthrough = StoryPlaythrough(start: story("gone"), among: up)

        #expect(playthrough.stories.map(\.id) == ["gone"])
        #expect(playthrough.isLast)
    }

    @Test func goingBackFromTheFirstChangesNothingAndGoingOnFromTheLastFinishes() {
        var playthrough = StoryPlaythrough(start: story("a"), among: up)

        #expect(playthrough.move(by: -1) == .unchanged)
        #expect(playthrough.current.id == "a")
        #expect(playthrough.move(by: 1) == .moved)
        #expect(playthrough.move(by: 1) == .moved)
        #expect(playthrough.isLast)
        #expect(playthrough.move(by: 1) == .finished)
        #expect(playthrough.current.id == "c")
    }

    @Test func aVideoThatEndsMovesOnToTheNextStory() {
        var playthrough = StoryPlaythrough(start: story("b", video: true), among: up)

        #expect(playthrough.videoEnded("b") == .moved)
        #expect(playthrough.current.id == "c")
    }

    @Test func theLastVideoEndingFinishes() {
        var playthrough = StoryPlaythrough(start: story("b", video: true), among: [story("a"), story("b", video: true)])

        #expect(playthrough.videoEnded("b") == .finished)
    }

    @Test func theEndOfAVideoAlreadyLeftChangesNothing() {
        var playthrough = StoryPlaythrough(start: story("b", video: true), among: up)
        _ = playthrough.move(by: -1)

        #expect(playthrough.videoEnded("b") == .unchanged)
        #expect(playthrough.current.id == "a")
    }

    @Test func muteIsKeptFromOneStoryToTheNext() {
        var playthrough = StoryPlaythrough(start: story("a"), among: up)
        playthrough.isMuted = true

        _ = playthrough.move(by: 1)

        #expect(playthrough.isMuted)
    }

    @Test func barsBeforeTheStoryAreFullAfterItEmptyAndItsOwnFillsAsItsVideoPlays() {
        let playthrough = StoryPlaythrough(start: story("b", video: true), among: up)

        #expect(playthrough.fill(at: 0, played: 0.4) == 1)
        #expect(playthrough.fill(at: 1, played: 0.4) == 0.4)
        #expect(playthrough.fill(at: 1, played: nil) == 1)
        #expect(playthrough.fill(at: 2, played: 0.4) == 0)
        #expect(playthrough.fill(at: 1, played: 1.3) == 1)
    }
}

@Suite("Story sound")
struct StorySoundTests {
    @Test func storiesMixWithOtherAudioWhenNoCallIsOn() {
        #expect(StorySound(during: .idle) == .mixed)
        #expect(StorySound(during: .ended(sid: "s1", reason: .finished(nil))) == .mixed)
    }

    @Test func storiesAreSilentWhileACallRingsOrIsOn() {
        let incoming = IncomingCall(from: JID(parsing: "bob@waddle.test/phone")!, sid: "s1", media: CallMedia(audio: true, video: false))
        #expect(StorySound(during: .incoming(incoming)) == .silencedByCall)
    }
}
