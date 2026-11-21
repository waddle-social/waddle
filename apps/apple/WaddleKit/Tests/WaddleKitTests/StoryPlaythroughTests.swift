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
    }

    @Test func aStoryNoLongerUpOpensAlone() {
        let playthrough = StoryPlaythrough(start: story("gone"), among: up)

        #expect(playthrough.stories.map(\.id) == ["gone"])
    }

    @Test func goingBackFromTheFirstStaysAndGoingOnFromTheLastCloses() {
        var playthrough = StoryPlaythrough(start: story("a"), among: up)

        #expect(playthrough.step(-1) == .show)
        #expect(playthrough.current.id == "a")
        #expect(playthrough.step(1) == .show)
        #expect(playthrough.step(1) == .show)
        #expect(playthrough.step(1) == .close)
        #expect(playthrough.current.id == "c")
    }

    @Test func aVideoThatEndsMovesOnToTheNextStory() {
        var playthrough = StoryPlaythrough(start: story("b", video: true), among: up)

        #expect(playthrough.videoEnded("b") == .show)
        #expect(playthrough.current.id == "c")
    }

    @Test func theLastVideoEndingClosesTheViewer() {
        var playthrough = StoryPlaythrough(start: story("b", video: true), among: [story("a"), story("b", video: true)])

        #expect(playthrough.videoEnded("b") == .close)
    }

    @Test func theEndOfAVideoAlreadyLeftIsIgnored() {
        var playthrough = StoryPlaythrough(start: story("b", video: true), among: up)
        _ = playthrough.step(-1)

        #expect(playthrough.videoEnded("b") == .show)
        #expect(playthrough.current.id == "a")
    }

    @Test func theNextStoryStartsPlayingFromTheBeginningAndStaysMuted() {
        var playthrough = StoryPlaythrough(start: story("a"), among: up)
        playthrough.isMuted = true
        playthrough.isPaused = true
        playthrough.progress = 0.6

        _ = playthrough.step(1)

        #expect(!playthrough.isPaused)
        #expect(playthrough.progress == 0)
        #expect(playthrough.isMuted)
    }
}
