import CoreVideo
import CryptoKit
import LiveKit
import WaddleKit
import XCTest
@testable import Waddle

/// The media seam and its SDK compatibility. Room tests need the SFU
/// named by `WADDLE_LIVEKIT_TEST_URL`; see the README for the local recipe.
@MainActor
final class LiveKitCallMediaTests: XCTestCase {
    /// A controller with a store of its own: a test must not read the device
    /// choices of whoever runs it, or leave its own behind.
    private func makeController() -> LiveKitCallMediaController {
        LiveKitCallMediaController(defaults: makeDefaults())
    }

    private func makeDefaults() -> UserDefaults {
        let name = "LiveKitCallMediaTests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: name) ?? .standard
        addTeardownBlock { defaults.removePersistentDomain(forName: name) }
        return defaults
    }

    func testTheBackgroundIsTheRoomUntilChosenAndIsKept() {
        let defaults = makeDefaults()
        let media = LiveKitCallMediaController(defaults: defaults)
        XCTAssertEqual(media.background, .off)
        // With no call there is no camera to give it to: it is kept for the next.
        media.setBackground(.blur)
        XCTAssertEqual(LiveKitCallMediaController(defaults: defaults).background, .blur)
        media.setBackground(.off)
        XCTAssertEqual(LiveKitCallMediaController(defaults: defaults).background, .off)
    }

    func testAudioProcessingIsAllOnUntilChosenAndIsKept() async {
        let defaults = makeDefaults()
        let media = LiveKitCallMediaController(defaults: defaults)
        XCTAssertEqual(media.audioProcessing, CallAudioProcessing())
        await media.setAudioProcessing(CallAudioProcessing(noiseSuppression: false, autoGainControl: false))

        let next = LiveKitCallMediaController(defaults: defaults)
        XCTAssertEqual(next.audioProcessing, CallAudioProcessing(noiseSuppression: false, echoCancellation: true, autoGainControl: false))
    }

    /// The microphone's own track takes a change: the same publication, on
    /// or muted, with nothing for the others to miss.
    func testAChangeOfProcessingIsTakenByTheMicrophonesTrackInPlace() async throws {
        let sfu = try testSFU()
        let room = "waddle-test-\(UUID().uuidString)"
        let alice = makeController()
        try await alice.connect(
            join: sfu.join(room: room, identity: "alice@waddle.test/mac"), media: CallMedia(audio: true, video: false),
            iceServers: []
        )
        try await until { alice.isMicrophoneOn }
        let publication = try XCTUnwrap(alice.microphonePublicationID)
        // The track was made with the choice, and took it again as it came on.
        try await until { alice.appliedAudioProcessing == CallAudioProcessing() }

        let quiet = CallAudioProcessing(noiseSuppression: false, autoGainControl: false)
        await alice.setAudioProcessing(quiet)
        XCTAssertEqual(alice.appliedAudioProcessing, quiet, "the SDK accepted the options on the live track")
        XCTAssertEqual(alice.microphonePublicationID, publication, "the microphone was not republished")
        XCTAssertTrue(alice.isMicrophoneOn)

        await alice.setMicrophone(false)
        try await until { !alice.isMicrophoneOn }
        await alice.setAudioProcessing(CallAudioProcessing())
        XCTAssertEqual(alice.appliedAudioProcessing, CallAudioProcessing(), "a muted microphone's track takes a change too")
        XCTAssertEqual(alice.microphonePublicationID, publication)
        XCTAssertFalse(alice.isMicrophoneOn, "a change never turns on a microphone that is off")
        await alice.disconnect()
        XCTAssertNil(alice.appliedAudioProcessing)
    }

    func testADeviceChoiceIsKeptForTheNextSessionAndCanBeCleared() async {
        let defaults = makeDefaults()
        let media = LiveKitCallMediaController(defaults: defaults)
        await media.chooseDevice(.camera, id: "a-camera-that-is-not-here")
        await media.chooseDevice(.microphone, id: "a-microphone-that-is-not-here")

        let next = LiveKitCallMediaController(defaults: defaults)
        await next.refreshDevices()
        XCTAssertEqual(next.devices.cameras.preferred, "a-camera-that-is-not-here")
        XCTAssertTrue(next.devices.cameras.isChoiceMissing, "a camera that is gone gives way to the system's")
        XCTAssertNil(next.devices.cameras.chosen)
        #if os(macOS)
        XCTAssertEqual(next.devices.microphones.preferred, "a-microphone-that-is-not-here")
        #endif

        await next.chooseDevice(.camera, id: nil)
        let last = LiveKitCallMediaController(defaults: defaults)
        await last.refreshDevices()
        XCTAssertNil(last.devices.cameras.preferred)
    }

    private func testSFU() throws -> TestSFU { try .fromEnvironment() }

    func testIdleMediaHasNoRosterOrVideo() {
        let media = makeController()
        let identity = "alice@waddle.test/mac"
        media.setVolume(1.5, of: identity)

        XCTAssertTrue(media.participants.isEmpty)
        XCTAssertTrue(media.activeSpeakersLoudestFirst.isEmpty)
        XCTAssertNil(media.dockCameraIdentities.local)
        XCTAssertNil(media.dockCameraIdentities.remote)
        XCTAssertNil(media.video(of: identity, source: .camera))
        XCTAssertNil(media.video(of: identity, source: .screen))
    }

    func testVolumeWritesCoalescePerTrackAndRunOffMain() async {
        let updates = CallVolumeUpdates()
        let first = NSObject()
        let second = NSObject()
        let latest = expectation(description: "latest update on the first track")
        let other = expectation(description: "update on the other track")

        updates.enqueue(for: first) { XCTFail("superseded volume was sent") }
        updates.enqueue(for: first) {
            XCTAssertFalse(Thread.isMainThread)
            latest.fulfill()
        }
        updates.enqueue(for: second) { other.fulfill() }

        await fulfillment(of: [latest, other], timeout: 2)
    }

    func testPendingVolumeWritesCanBeDiscardedWhenTheRoomCloses() async {
        let updates = CallVolumeUpdates()
        let oldTrack = NSObject()
        let newTrack = NSObject()
        let applied = expectation(description: "new room receives volume")

        updates.enqueue(for: oldTrack) { XCTFail("old room received volume") }
        updates.removeAll()
        updates.enqueue(for: newTrack) { applied.fulfill() }

        await fulfillment(of: [applied], timeout: 2)
    }

    func testJoinsARoomAndLeaves() async throws {
        let sfu = try testSFU()
        let media = makeController()
        let join = sfu.join(room: "waddle-test-\(UUID().uuidString)", identity: "alice@waddle.test/mac")
        XCTAssertNil(join.grantDefect)

        try await media.connect(join: join, media: CallMedia(audio: false, video: false), iceServers: [])
        await media.disconnect()
        // A second call reuses the controller.
        try await media.connect(join: join, media: CallMedia(audio: false, video: false), iceServers: [])
        await media.disconnect()
    }

    func testThePhaseFollowsTheConnection() async throws {
        let sfu = try testSFU()
        let media = makeController()
        XCTAssertEqual(media.phase, .idle)

        try await media.connect(
            join: sfu.join(room: "waddle-test-\(UUID().uuidString)", identity: "alice@waddle.test/mac"),
            media: CallMedia(audio: false, video: false), iceServers: []
        )
        XCTAssertEqual(media.phase, .connected)
        XCTAssertFalse(media.isMicrophoneOn, "a call without audio publishes no microphone")
        await media.disconnect()
        XCTAssertEqual(media.phase, .idle)
    }

    func testAloneInARoomTheParticipantsAreJustUs() async throws {
        let sfu = try testSFU()
        let media = makeController()
        try await media.connect(
            join: sfu.join(room: "waddle-test-\(UUID().uuidString)", identity: "alice@waddle.test/mac"),
            media: CallMedia(audio: false, video: false), iceServers: []
        )
        XCTAssertEqual(media.participants, [
            CallMediaParticipant(
                identity: "alice@waddle.test/mac", isLocal: true, isCameraOn: false,
                hasMicrophoneTrack: false, isSpeaking: false, volume: 1
            ),
        ])
        media.setVolume(0, of: "alice@waddle.test/mac")
        XCTAssertEqual(media.participants.first?.volume, 1)
        XCTAssertNil(media.video(of: "alice@waddle.test/mac", source: .screen))
        await media.disconnect()
        XCTAssertTrue(media.participants.isEmpty, "a forgotten room has no participants")
    }

    /// The SDK reports joins and leaves through optional delegate methods,
    /// which a wrong signature would silently never receive.
    func testRemotesAppearInJoinOrderAndAreRemovedWhenTheyLeave() async throws {
        let sfu = try testSFU()
        let room = "waddle-test-\(UUID().uuidString)"
        let alice = makeController()
        let bob = makeController()
        let carol = makeController()
        let none = CallMedia(audio: false, video: false)
        let own = "alice@waddle.test/mac"
        let first = "carol@waddle.test/phone"
        let second = "bob@waddle.test/phone"
        try await alice.connect(join: sfu.join(room: room, identity: own), media: none, iceServers: [])
        try await carol.connect(join: sfu.join(room: room, identity: first), media: none, iceServers: [])
        try await bob.connect(join: sfu.join(room: room, identity: second), media: none, iceServers: [])

        try await until { alice.participants.map(\.identity) == [own, first, second] }
        await carol.disconnect()
        try await until { alice.participants.map(\.identity) == [own, second] }
        await bob.disconnect()
        try await until { alice.participants.map(\.identity) == [own] }
        await alice.disconnect()
    }

    func testRemoteGainSurvivesRoomReplacementUntilTheCallChoicesReset() async throws {
        let sfu = try testSFU()
        let room = "waddle-test-\(UUID().uuidString)"
        let alice = makeController()
        let bob = makeController()
        let aliceJoin = sfu.join(room: room, identity: "alice@waddle.test/mac")
        let bobJoin = sfu.join(room: room, identity: "bob@waddle.test/phone")
        let none = CallMedia(audio: false, video: false)

        try await bob.connect(join: bobJoin, media: none, iceServers: [])
        try await alice.connect(join: aliceJoin, media: none, iceServers: [])
        try await until { alice.participants.last?.identity == bobJoin.identity }
        alice.setVolume(3, of: bobJoin.identity)
        XCTAssertEqual(alice.participants.last?.volume, 2)
        await alice.disconnect()

        try await alice.connect(join: aliceJoin, media: none, iceServers: [])
        try await until { alice.participants.last?.identity == bobJoin.identity }
        XCTAssertEqual(alice.participants.last?.volume, 2)
        await alice.disconnect()

        alice.forgetCaptureChoices()
        try await alice.connect(join: aliceJoin, media: none, iceServers: [])
        try await until { alice.participants.last?.identity == bobJoin.identity }
        XCTAssertEqual(alice.participants.last?.volume, 1)
        await alice.disconnect()
        await bob.disconnect()
    }

    func testResetAllReachesSomeoneWhoHasLeftAndComesBack() async throws {
        let sfu = try testSFU()
        let room = "waddle-test-\(UUID().uuidString)"
        let alice = makeController()
        let bob = makeController()
        let bobJoin = sfu.join(room: room, identity: "bob@waddle.test/phone")
        let none = CallMedia(audio: false, video: false)

        try await bob.connect(join: bobJoin, media: none, iceServers: [])
        try await alice.connect(join: sfu.join(room: room, identity: "alice@waddle.test/mac"), media: none, iceServers: [])
        try await until { alice.participants.last?.identity == bobJoin.identity }
        XCTAssertFalse(alice.hasChangedVolumes)
        alice.setVolume(0, of: bobJoin.identity)
        await bob.disconnect()
        try await until { alice.participants.count == 1 }
        XCTAssertTrue(alice.hasChangedVolumes, "a silenced person who left is still silenced")

        alice.resetVolumes()
        XCTAssertFalse(alice.hasChangedVolumes)
        try await bob.connect(join: bobJoin, media: none, iceServers: [])
        try await until { alice.participants.last?.identity == bobJoin.identity }
        XCTAssertEqual(alice.participants.last?.volume, 1)
        await alice.disconnect()
        await bob.disconnect()
    }

    /// Real readings from the engine for a stream we receive: they carry a
    /// byte count, and a timestamp the tracker reads as now.
    func testAMeasuredStreamHasReadingsTheTrackerCanTime() async throws {
        let sfu = try testSFU()
        let room = "waddle-test-\(UUID().uuidString)"
        let alice = makeController()
        let bobJoin = sfu.join(room: room, identity: "bob@waddle.test/web")
        let bob = Room()
        try await bob.connect(url: bobJoin.url, token: bobJoin.token)
        try await alice.connect(
            join: sfu.join(room: room, identity: "alice@waddle.test/mac"), media: CallMedia(audio: false, video: false),
            iceServers: []
        )
        let screen = await LocalVideoTrack.createBufferTrack(source: .screenShareVideo)
        let frames = Task {
            var buffer: CVPixelBuffer?
            CVPixelBufferCreate(nil, 640, 360, kCVPixelFormatType_32BGRA, nil, &buffer)
            while !Task.isCancelled, let buffer {
                (screen.capturer as? BufferCapturer)?.capture(buffer)
                try? await Task.sleep(for: .milliseconds(100))
            }
        }
        defer { frames.cancel() }
        _ = try await bob.localParticipant.publish(videoTrack: screen)
        try await until { alice.participants.last?.screenPublicationID != nil }
        let unmeasured = await alice.videoSamples()
        XCTAssertTrue(unmeasured.isEmpty, "nothing is measured until someone looks")

        await alice.setMeasuresVideo(true)
        var sample: CallVideoSample?
        let deadline = ContinuousClock.now + .seconds(15)
        while sample?.bytes == nil, ContinuousClock.now < deadline {
            sample = await alice.videoSamples().first
            try await Task.sleep(for: .milliseconds(200))
        }
        let reading = try XCTUnwrap(sample)
        XCTAssertNotNil(reading.bytes)
        XCTAssertEqual(reading.key, "\(bobJoin.identity):screen:received")
        XCTAssertEqual(reading.label, "bob")
        XCTAssertEqual(reading.direction, .received)
        let taken = CallVideoStatsTracker.seconds(reading.timestamp)
        XCTAssertEqual(taken, Date().timeIntervalSince1970, accuracy: 3600, "the engine's timestamp is read in the right unit")
        // A per-track report carries the path the media takes, not only the stream's own counters.
        XCTAssertNotNil(reading.candidateType, "the network path is part of a track's report")
        XCTAssertNotNil(reading.transport)
        if let codec = reading.codec { XCTAssertNotEqual(codec.lowercased(), "video/rtx", "the stream's own codec, never the retransmission one") }

        // A second window opens its settings and the first closes: the one still open keeps its figures.
        await alice.setMeasuresVideo(true)
        await alice.setMeasuresVideo(false)
        let stillWatched = await alice.videoSamples()
        XCTAssertFalse(stillWatched.isEmpty, "measuring goes on while anyone is looking")

        await alice.setMeasuresVideo(false)
        let after = await alice.videoSamples()
        XCTAssertTrue(after.isEmpty)
        // One close too many does not leave the count below nothing.
        await alice.setMeasuresVideo(false)
        await alice.setMeasuresVideo(true)
        let again = await alice.videoSamples()
        XCTAssertFalse(again.isEmpty)
        await alice.setMeasuresVideo(false)
        await alice.disconnect()
        await bob.disconnect()
    }

    func testAScreenSomeoneSharesIsTheirsToShowUntilTheyStop() async throws {
        let sfu = try testSFU()
        let room = "waddle-test-\(UUID().uuidString)"
        let alice = makeController()
        let bobJoin = sfu.join(room: room, identity: "bob@waddle.test/web")
        // Bob is a plain SDK client, as a browser sharing its screen is.
        let bob = Room()
        try await bob.connect(url: bobJoin.url, token: bobJoin.token)
        try await alice.connect(
            join: sfu.join(room: room, identity: "alice@waddle.test/mac"), media: CallMedia(audio: false, video: false),
            iceServers: []
        )
        try await until { alice.participants.last?.identity == bobJoin.identity }
        XCTAssertNil(alice.participants.last?.screenPublicationID)
        XCTAssertNil(alice.video(of: bobJoin.identity, source: .screen))

        let screen = await LocalVideoTrack.createBufferTrack(source: .screenShareVideo)
        let frames = Task {
            var buffer: CVPixelBuffer?
            CVPixelBufferCreate(nil, 640, 360, kCVPixelFormatType_32BGRA, nil, &buffer)
            while !Task.isCancelled, let buffer {
                (screen.capturer as? BufferCapturer)?.capture(buffer)
                try? await Task.sleep(for: .milliseconds(100))
            }
        }
        defer { frames.cancel() }
        let publication = try await bob.localParticipant.publish(videoTrack: screen)
        try await until { alice.participants.last?.screenPublicationID != nil }
        XCTAssertEqual(alice.participants.last?.screenPublicationID, publication.sid.stringValue)
        XCTAssertNotNil(alice.video(of: bobJoin.identity, source: .screen))
        XCTAssertEqual(alice.dockCameraIdentities.remoteScreen, bobJoin.identity)
        // A screen is not a camera.
        XCTAssertEqual(alice.participants.last?.isCameraOn, false)
        XCTAssertNil(alice.video(of: bobJoin.identity, source: .camera))

        try await bob.localParticipant.unpublish(publication: publication)
        try await until { alice.participants.last?.screenPublicationID == nil }
        XCTAssertNil(alice.video(of: bobJoin.identity, source: .screen))
        XCTAssertNil(alice.dockCameraIdentities.remoteScreen)
        await alice.disconnect()
        await bob.disconnect()
    }

    private func until(timeout: Duration = .seconds(10), _ condition: () -> Bool) async throws {
        let deadline = ContinuousClock.now + timeout
        while !condition() {
            guard ContinuousClock.now < deadline else { return XCTFail("timed out") }
            try await Task.sleep(for: .milliseconds(50))
        }
    }

    func testJoinsWithAdvertisedIceServers() async throws {
        let sfu = try testSFU()
        let media = makeController()
        let join = sfu.join(room: "waddle-test-\(UUID().uuidString)", identity: "alice@waddle.test/mac")
        // What XEP-0215 would advertise, mapped the way the call store maps it.
        let advertised = WaddleKit.IceServer.servers(from: [
            ExternalService(kind: .stun, host: "127.0.0.1", port: 3478),
            ExternalService(kind: .turn, host: "127.0.0.1", port: 3478, transport: .udp, username: "u", password: "p"),
        ])

        try await media.connect(join: join, media: CallMedia(audio: false, video: false), iceServers: advertised)
        await media.disconnect()
    }

    func testATokenTheSFUDidNotIssueCannotJoin() async throws {
        let sfu = try testSFU()
        let media = makeController()
        let forged = TestSFU(url: sfu.url, key: sfu.key, secret: "not-the-secret")
            .join(room: "waddle-test-\(UUID().uuidString)", identity: "mallory@waddle.test/mac")
        do {
            try await media.connect(join: forged, media: CallMedia(audio: false, video: false), iceServers: [])
            XCTFail("a forged token joined the room")
            await media.disconnect()
        } catch {
            XCTAssertFalse(error is CancellationError, "the SFU refused the join; nothing cancelled it")
        }
    }

    func testTheSFUClosingTheRoomIsReportedAsAFailure() async throws {
        let sfu = try testSFU()
        let media = makeController()
        let room = "waddle-test-\(UUID().uuidString)"
        let dropped = expectation(description: "the media session reported its death")
        media.onFailure = { dropped.fulfill() }

        try await media.connect(
            join: sfu.join(room: room, identity: "alice@waddle.test/mac"),
            media: CallMedia(audio: false, video: false), iceServers: []
        )
        try await sfu.deleteRoom(room)
        await fulfillment(of: [dropped], timeout: 15)
        await media.disconnect()
    }

    func testLeavingIsNotReportedAsAFailure() async throws {
        let sfu = try testSFU()
        let media = makeController()
        var failures = 0
        media.onFailure = { failures += 1 }

        try await media.connect(
            join: sfu.join(room: "waddle-test-\(UUID().uuidString)", identity: "alice@waddle.test/mac"),
            media: CallMedia(audio: false, video: false), iceServers: []
        )
        await media.disconnect()
        try await Task.sleep(for: .seconds(1))
        XCTAssertEqual(failures, 0)
    }
}
