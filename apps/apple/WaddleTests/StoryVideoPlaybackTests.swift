import AVFoundation
import XCTest
@testable import Waddle

/// A story video's player is the one to say whether it plays: a pause the
/// system makes shows, and the button acts on it. Played for real, from a
/// clip written here.
final class StoryVideoPlaybackTests: ClipTestCase {
    func testAPauseTheSystemMakesShowsAndTheButtonPlaysAgain() async throws {
        let playback = StoryVideoPlayback(storyID: "s1", url: try await clip(), isMuted: true)
        let running = Task { await playback.run() }
        defer { running.cancel() }
        try await waitUntil { !playback.isPaused || playback.progress > 0 }

        // As the app going to the background or an interruption would.
        playback.player?.pause()
        try await waitUntil { playback.isPaused }

        playback.togglePause()
        XCTAssertFalse(playback.isPaused)
    }

    func testAVideoThatPlaysToItsEndSaysSo() async throws {
        let playback = StoryVideoPlayback(storyID: "s1", url: try await clip(), isMuted: true)
        var ended = false
        playback.onEnd = { ended = true }
        let running = Task { await playback.run() }
        defer { running.cancel() }

        try await waitUntil(seconds: 10) { ended }
        XCTAssertGreaterThan(playback.progress, 0.5)
    }

    func testAFileThatDoesNotPlayFails() async throws {
        let file = temporaryFile("mp4")
        try Data("not a movie".utf8).write(to: file)
        let playback = StoryVideoPlayback(storyID: "s1", url: file, isMuted: true)
        let running = Task { await playback.run() }
        defer { running.cancel() }

        try await waitUntil { playback.didFail }
        await running.value
        XCTAssertTrue(playback.didFail)
        XCTAssertFalse(playback.isLoading)
    }

    private func waitUntil(seconds: Double = 5, _ condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(seconds)
        while !condition() {
            guard Date() < deadline else { return XCTFail("Timed out") }
            try await Task.sleep(for: .milliseconds(50))
        }
    }
}
