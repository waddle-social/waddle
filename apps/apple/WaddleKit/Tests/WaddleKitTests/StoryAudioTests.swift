import Testing
@testable import WaddleKit

/// An audio session that records what story playback did to it. A call
/// changes it behind the story's back, as `SystemCallAudio` does.
@MainActor
private final class FakeAudioSession: StoryAudioSession {
    var setting = "soloAmbient"
    var changes: [String] = []

    var isAmbient: Bool { setting == "ambient" }
    func currentSetting() -> String { setting }
    func setAmbient() { apply("ambient") }
    func restore(_ setting: String) { apply(setting) }

    private func apply(_ next: String) {
        setting = next
        changes.append(next)
    }
}

@MainActor
@Suite("Story audio session")
struct StoryAudioTests {
    @Test func storiesMixOutsideACallAndPutTheSessionBackOnClose() {
        let session = FakeAudioSession()
        var audio = StoryAudio(session: session)

        audio.update(.mixed)
        #expect(session.setting == "ambient")
        audio.close()

        #expect(session.changes == ["ambient", "soloAmbient"])
    }

    @Test func aCallInTheMiddleIsLeftAloneAndStoriesMixAgainAfterIt() {
        let session = FakeAudioSession()
        var audio = StoryAudio(session: session)

        audio.update(.mixed)
        audio.update(.silencedByCall)
        session.setting = "playAndRecord"
        audio.update(.silencedByCall)
        #expect(session.changes == ["ambient"])

        audio.update(.mixed)
        #expect(session.setting == "ambient")
        audio.close()

        // What was there before the story, not what the call left.
        #expect(session.changes == ["ambient", "ambient", "soloAmbient"])
    }

    @Test func closingDuringACallLeavesTheCallsSession() {
        let session = FakeAudioSession()
        var audio = StoryAudio(session: session)

        audio.update(.mixed)
        audio.update(.silencedByCall)
        session.setting = "playAndRecord"
        audio.close()

        #expect(session.setting == "playAndRecord")
    }

    @Test func openingDuringACallTouchesNothing() {
        let session = FakeAudioSession()
        session.setting = "playAndRecord"
        var audio = StoryAudio(session: session)

        audio.update(.silencedByCall)
        audio.close()

        #expect(session.changes == [])
    }

    @Test func aSessionAlreadyAmbientIsNotChanged() {
        let session = FakeAudioSession()
        session.setting = "ambient"
        var audio = StoryAudio(session: session)

        audio.update(.mixed)
        audio.update(.mixed)
        audio.close()

        #expect(session.changes == [])
    }
}
