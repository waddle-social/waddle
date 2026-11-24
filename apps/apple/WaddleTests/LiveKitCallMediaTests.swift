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

    func testAudioProcessingIsAllOnUntilChosenAndIsKept() async {
        let defaults = makeDefaults()
        let media = LiveKitCallMediaController(defaults: defaults)
        XCTAssertEqual(media.audioProcessing, CallAudioProcessing())
        await media.setAudioProcessing(CallAudioProcessing(noiseSuppression: false, autoGainControl: false))

        let next = LiveKitCallMediaController(defaults: defaults)
        XCTAssertEqual(next.audioProcessing, CallAudioProcessing(noiseSuppression: false, echoCancellation: true, autoGainControl: false))
    }

    /// The SDK fixes a track's processing when it makes it, so a change
    /// while the microphone is on is a new track that carries the choice.
    func testAChangeOfProcessingReplacesALiveMicrophoneAndLeavesAMutedOneOff() async throws {
        let sfu = try testSFU()
        let room = "waddle-test-\(UUID().uuidString)"
        let alice = makeController()
        try await alice.connect(
            join: sfu.join(room: room, identity: "alice@waddle.test/mac"), media: CallMedia(audio: true, video: false),
            iceServers: []
        )
        try await until { alice.isMicrophoneOn }

        await alice.setAudioProcessing(CallAudioProcessing(noiseSuppression: false))
        XCTAssertTrue(alice.isMicrophoneOn, "the microphone is on again once it has been replaced")
        XCTAssertEqual(alice.audioProcessing.noiseSuppression, false)

        await alice.setMicrophone(false)
        try await until { !alice.isMicrophoneOn }
        await alice.setAudioProcessing(CallAudioProcessing())
        XCTAssertFalse(alice.isMicrophoneOn, "a change never turns on a microphone that is off")
        await alice.setMicrophone(true)
        try await until { alice.isMicrophoneOn }
        await alice.disconnect()
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

    private func testSFU() throws -> TestSFU {
        let environment = ProcessInfo.processInfo.environment
        guard let url = environment["WADDLE_LIVEKIT_TEST_URL"], !url.isEmpty else {
            throw XCTSkip("WADDLE_LIVEKIT_TEST_URL is not set")
        }
        return TestSFU(
            url: url,
            key: environment["WADDLE_LIVEKIT_TEST_KEY"] ?? "devkey",
            secret: environment["WADDLE_LIVEKIT_TEST_SECRET"] ?? "secret"
        )
    }

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

/// Mints the tokens Waddle's server would issue, and administers rooms.
private struct TestSFU {
    let url: String
    let key: String
    let secret: String

    func join(room: String, identity: String) -> CallJoin {
        CallJoin(url: url, room: room, identity: identity, token: token(subject: identity, grant: ["roomJoin": true, "room": room]))
    }

    func deleteRoom(_ room: String) async throws {
        var components = URLComponents(string: url)!
        components.scheme = components.scheme == "wss" ? "https" : "http"
        components.path = "/twirp/livekit.RoomService/DeleteRoom"
        var request = URLRequest(url: components.url!)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.setValue("Bearer \(token(subject: "admin", grant: ["roomCreate": true]))", forHTTPHeaderField: "Authorization")
        request.httpBody = try JSONSerialization.data(withJSONObject: ["room": room])
        let (_, response) = try await URLSession.shared.data(for: request)
        XCTAssertEqual((response as? HTTPURLResponse)?.statusCode, 200)
    }

    private func token(subject: String, grant: [String: Any]) -> String {
        let now = Int(Date().timeIntervalSince1970)
        let header = segment(["alg": "HS256", "typ": "JWT"])
        let payload = segment(["iss": key, "sub": subject, "nbf": now - 10, "exp": now + 600, "video": grant])
        let signature = HMAC<SHA256>.authenticationCode(for: Data("\(header).\(payload)".utf8), using: SymmetricKey(data: Data(secret.utf8)))
        return "\(header).\(payload).\(base64URL(Data(signature)))"
    }

    private func segment(_ object: [String: Any]) -> String {
        base64URL((try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])) ?? Data())
    }

    private func base64URL(_ data: Data) -> String {
        data.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}
