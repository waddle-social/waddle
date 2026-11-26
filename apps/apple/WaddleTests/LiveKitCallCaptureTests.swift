import AVFoundation
import LiveKit
import WaddleKit
import XCTest
@testable import Waddle

/// Our own camera and our own shared screen, through the controller, with
/// capture that needs no hardware (`BufferCaptureSources`). They pin what
/// the controller does today, ahead of its being taken apart (#2822), and
/// need the SFU named by `WADDLE_LIVEKIT_TEST_URL`.
@MainActor
final class LiveKitCallCaptureTests: XCTestCase {
    private struct Call {
        let media: LiveKitCallMediaController
        let capture: BufferCaptureSources
        /// A plain SDK client in the same room, as a browser is.
        let peer: Room
        let identity: String
    }

    private func call(video: Bool = false, configure: (BufferCaptureSources) -> Void = { _ in }) async throws -> Call {
        let sfu = try TestSFU.fromEnvironment()
        let room = "waddle-test-\(UUID().uuidString)"
        let name = "LiveKitCallCaptureTests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: name) ?? .standard
        let capture = BufferCaptureSources()
        configure(capture)
        let media = LiveKitCallMediaController(defaults: defaults, capture: capture)
        let peer = Room()
        let identity = "alice@waddle.test/mac"
        addTeardownBlock { @MainActor in
            capture.stop()
            await media.disconnect()
            await peer.disconnect()
            defaults.removePersistentDomain(forName: name)
        }
        let peerJoin = sfu.join(room: room, identity: "bob@waddle.test/web")
        try await peer.connect(url: peerJoin.url, token: peerJoin.token)
        try await media.connect(
            join: sfu.join(room: room, identity: identity), media: CallMedia(audio: false, video: video), iceServers: []
        )
        try await until { peer.remoteParticipants.count == 1 }
        return Call(media: media, capture: capture, peer: peer, identity: identity)
    }

    private func until(timeout: Duration = .seconds(10), _ condition: () -> Bool) async throws {
        let deadline = ContinuousClock.now + timeout
        while !condition() {
            guard ContinuousClock.now < deadline else { return XCTFail("timed out") }
            try await Task.sleep(for: .milliseconds(50))
        }
    }

    /// What the other side has of ours from `source`, muted or not.
    private func published(_ source: Track.Source, by call: Call) -> [RemoteTrackPublication] {
        (call.peer.remoteParticipants.values.first?.trackPublications.values.compactMap { $0 as? RemoteTrackPublication } ?? [])
            .filter { $0.source == source }
    }

    // MARK: - The camera

    func testACallWithVideoAsksOnceAndPublishesOneCameraWithItsBackgroundProcessor() async throws {
        let call = try await call(video: true)
        try await until { self.published(.camera, by: call).count == 1 }
        XCTAssertTrue(call.media.isCameraOn)
        XCTAssertFalse(call.media.isCameraBlocked)
        XCTAssertEqual(call.capture.asked, [.video])
        XCTAssertEqual(call.capture.cameras.count, 1)
        XCTAssertNotNil(call.capture.cameras.first?.capturer.processor, "frames never leave without the processor")
    }

    func testACameraTurnedOffIsMutedAndTurnedOnAgainIsTheSameCamera() async throws {
        let call = try await call(video: true)
        try await until { self.published(.camera, by: call).count == 1 }

        await call.media.setCamera(false)
        XCTAssertFalse(call.media.isCameraOn)
        try await until { self.published(.camera, by: call).first?.isMuted == true }

        await call.media.setCamera(true)
        XCTAssertTrue(call.media.isCameraOn)
        try await until { self.published(.camera, by: call).first?.isMuted == false }
        XCTAssertEqual(call.capture.cameras.count, 1, "unmuted, not made again")
        XCTAssertEqual(published(.camera, by: call).count, 1)
    }

    func testACameraTheSystemRefusesIsBlockedAndNeverMade() async throws {
        let call = try await call(video: true) { $0.allows[.video] = false }
        XCTAssertFalse(call.media.isCameraOn)
        XCTAssertTrue(call.media.isCameraBlocked)
        XCTAssertTrue(call.capture.cameras.isEmpty)

        // Allowed since, in the system's settings: asking again turns it on.
        call.capture.allows[.video] = true
        await call.media.setCamera(true)
        XCTAssertTrue(call.media.isCameraOn)
        XCTAssertFalse(call.media.isCameraBlocked)
        XCTAssertEqual(call.capture.cameras.count, 1)
    }

    func testACallWithoutVideoAsksForNothingUntilTheCameraIsTurnedOn() async throws {
        let call = try await call()
        XCTAssertTrue(call.capture.asked.isEmpty)
        XCTAssertFalse(call.media.isCameraOn)

        await call.media.setCamera(true)
        XCTAssertEqual(call.capture.asked, [.video])
        XCTAssertTrue(call.media.isCameraOn)
        try await until { self.published(.camera, by: call).count == 1 }
    }

    /// The camera the call itself started, with no choice made by the person (#2823).
    func testCheckingAgainStartsACameraTheCallStartedAndTheBlurGaveNoPictureTo() async throws {
        let call = try await call(video: true) {
            $0.feedsCameras = false
        }
        call.media.setBackground(.blur)
        // No frame, so the SDK gives up on the track and there is no camera.
        try await until(timeout: .seconds(40)) { call.capture.cameras.count == 1 && call.media.isBackgroundFailing }
        XCTAssertFalse(call.media.isCameraOn)

        call.capture.feedsCameras = true
        await call.media.restartBackground()
        XCTAssertEqual(call.capture.cameras.count, 2, "checking again starts the camera again")
    }

    func testAPromptAnsweredAfterTheCallEndedSaysNothingOfTheCamera() async throws {
        let call = try await call {
            $0.allows[.video] = false
            $0.holdsAnswers = true
        }
        let turningOn = Task { await call.media.setCamera(true) }
        try await until { call.capture.asked == [.video] }

        await call.media.disconnect()
        call.capture.answerPrompts()
        await turningOn.value

        XCTAssertFalse(call.media.isCameraBlocked, "the answer was for a call that is over")
        XCTAssertTrue(call.media.mediaRefusals.isEmpty)
    }

    // MARK: - Our screen

    #if os(macOS)
    func testAScreenWePickIsSharedUntilWeStop() async throws {
        let call = try await call()
        let listed = await call.media.screenSources()
        let sources = try XCTUnwrap(listed)
        XCTAssertEqual(sources.screens.map(\.id), ["screen:1"])
        XCTAssertEqual(call.capture.screenPrompts, 0, "already allowed: nothing to prompt for")
        XCTAssertFalse(call.media.isScreenShareOn)

        await call.media.shareScreen(sources.screens[0])
        XCTAssertTrue(call.media.isScreenShareOn)
        XCTAssertFalse(call.media.isScreenShareStarting)
        XCTAssertFalse(call.media.didScreenShareFail)
        try await until { self.published(.screenShareVideo, by: call).count == 1 }
        XCTAssertTrue(published(.camera, by: call).isEmpty, "a screen is not a camera")

        await call.media.stopScreenShare()
        XCTAssertFalse(call.media.isScreenShareOn)
        try await until { self.published(.screenShareVideo, by: call).isEmpty }
    }

    func testASecondPickReplacesTheFirst() async throws {
        let call = try await call { $0.screens = [1, 2] }
        let listed = await call.media.screenSources()
        let sources = try XCTUnwrap(listed)
        XCTAssertEqual(sources.screens.map(\.id), ["screen:1", "screen:2"])

        await call.media.shareScreen(sources.screens[0])
        try await until { self.published(.screenShareVideo, by: call).count == 1 }
        let first = published(.screenShareVideo, by: call).first?.sid

        await call.media.shareScreen(sources.screens[1])
        XCTAssertTrue(call.media.isScreenShareOn)
        try await until {
            let now = self.published(.screenShareVideo, by: call)
            return now.count == 1 && now.first?.sid != first
        }
        XCTAssertEqual(call.capture.screenTracks.count, 2)
    }

    func testAScreenThatIsGoneWhenPickedFailsAndNothingIsShared() async throws {
        let call = try await call()
        let listed = await call.media.screenSources()
        let sources = try XCTUnwrap(listed)
        // Unplugged between the list and the pick.
        call.capture.screens = []

        await call.media.shareScreen(sources.screens[0])
        XCTAssertFalse(call.media.isScreenShareOn)
        XCTAssertTrue(call.media.didScreenShareFail)
        XCTAssertTrue(call.capture.screenTracks.isEmpty)

        // Asking for the list again puts the failure behind us.
        _ = await call.media.screenSources()
        XCTAssertFalse(call.media.didScreenShareFail)
    }

    func testARefusedScreenRecordingPromptsAndIsBlockedUntilAllowed() async throws {
        let call = try await call {
            $0.isScreenCaptureAllowed = false
            $0.screens = nil
        }
        let refused = await call.media.screenSources()
        XCTAssertNil(refused)
        XCTAssertEqual(call.capture.screenPrompts, 1)
        XCTAssertTrue(call.media.isScreenShareBlocked)

        call.capture.isScreenCaptureAllowed = true
        call.capture.screens = [1]
        let allowed = await call.media.screenSources()
        XCTAssertEqual(allowed?.screens.count, 1)
        XCTAssertFalse(call.media.isScreenShareBlocked)
        XCTAssertEqual(call.capture.screenPrompts, 1)
    }

    func testStoppingOvertakesAShareThatIsStillStarting() async throws {
        let call = try await call { $0.screenStartDelay = .milliseconds(500) }
        let listed = await call.media.screenSources()
        let sources = try XCTUnwrap(listed)

        let media = call.media
        let starting = Task { await media.shareScreen(sources.screens[0]) }
        try await until { media.isScreenShareStarting }
        await media.stopScreenShare()
        await starting.value

        XCTAssertFalse(call.media.isScreenShareOn)
        XCTAssertFalse(call.media.isScreenShareStarting)
        try await Task.sleep(for: .milliseconds(500))
        XCTAssertTrue(published(.screenShareVideo, by: call).isEmpty)
    }
    #endif
}
